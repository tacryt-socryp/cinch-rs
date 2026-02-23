//! Declarative parallel context gathering with deadline tracking.
//!
//! [`ContextGatherer`] runs multiple async tasks in parallel, collecting
//! results into a caller-provided struct.  Each task has a name (for
//! logging and progress reporting), an optional per-task timeout, an async
//! future, and a setter closure that stores the result.
//!
//! Progress is reported through the [`GatherObserver`] trait, which any UI
//! backend (TUI, web, native) can implement.  A built-in
//! [`UiGatherObserver`] bridges to [`UiState`](crate::ui::UiState) for
//! the common case.
//!
//! # Example
//!
//! ```ignore
//! use cinch_rs::agent::gather::ContextGatherer;
//! use std::time::Duration;
//!
//! #[derive(Default)]
//! struct MyContext {
//!     market: String,
//!     docs: String,
//! }
//!
//! let mut ctx = MyContext::default();
//! ContextGatherer::new(Duration::from_secs(30))
//!     .task("market", Duration::from_secs(15),
//!         async { fetch_market_data().await },
//!         |r, ctx| ctx.market = r,
//!     )
//!     .task("docs", Duration::ZERO,
//!         async { load_docs().await },
//!         |r, ctx| ctx.docs = r,
//!     )
//!     .run(&mut ctx)
//!     .await;
//! ```

use std::any::Any;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::task::JoinSet;
use tracing::{info, warn};

// ── Progress events & observer trait ────────────────────────────────

/// Structured events emitted by [`ContextGatherer`] during execution.
///
/// These events carry enough detail for any UI backend to render
/// meaningful progress — without coupling the gatherer to a specific
/// state model.
#[derive(Debug, Clone)]
pub enum GatherEvent<'a> {
    /// Gathering has started.
    Started { total: usize, tasks: &'a [String] },
    /// A single task completed successfully.
    TaskDone {
        name: &'a str,
        pending: Vec<&'a str>,
        done: usize,
        total: usize,
    },
    /// A single task exceeded its per-task timeout.
    TaskTimeout { name: &'a str },
    /// The global deadline was reached; listed tasks are abandoned.
    Deadline { abandoned: Vec<&'a str> },
    /// All tasks completed within the deadline.
    Finished,
}

impl GatherEvent<'_> {
    /// Render this event as a single-line phase string suitable for
    /// status bars and log messages.
    ///
    /// Uses a caller-provided prefix (e.g. "Pre-computing context").
    pub fn phase_string(&self, prefix: &str) -> String {
        match self {
            Self::Started { total, .. } => format!("{prefix} (0/{total})"),
            Self::TaskDone {
                pending,
                done,
                total,
                ..
            } => {
                if pending.is_empty() {
                    format!("{prefix} (done)")
                } else {
                    format!(
                        "{prefix} ({done}/{total}) — waiting: {}",
                        pending.join(", ")
                    )
                }
            }
            Self::TaskTimeout { name } => format!("{prefix}: {name} timed out"),
            Self::Deadline { abandoned } => {
                format!("{prefix}: deadline — abandoning: {}", abandoned.join(", "))
            }
            Self::Finished => format!("{prefix} (done)"),
        }
    }
}

/// Observer for [`ContextGatherer`] progress events.
///
/// Implement this trait to receive structured progress updates from the
/// gatherer.  Any UI backend — TUI, web dashboard, native app — can
/// provide its own implementation.
///
/// A built-in [`UiGatherObserver`] is provided for the common case of
/// writing to [`UiState`](crate::ui::UiState).
pub trait GatherObserver: Send + Sync {
    /// Called for each progress event during gathering.
    fn on_gather_event(&self, event: &GatherEvent<'_>);
}

/// [`GatherObserver`] that updates [`UiState`](crate::ui::UiState).
///
/// This is the standard observer for agents that use the harness UI
/// layer (TUI or web).  It formats each event into a phase string and
/// writes it via [`update_phase`](crate::ui::update_phase).
pub struct UiGatherObserver {
    state: Arc<Mutex<crate::ui::UiState>>,
    prefix: String,
}

impl UiGatherObserver {
    pub fn new(state: Arc<Mutex<crate::ui::UiState>>, prefix: impl Into<String>) -> Self {
        Self {
            state,
            prefix: prefix.into(),
        }
    }
}

impl GatherObserver for UiGatherObserver {
    fn on_gather_event(&self, event: &GatherEvent<'_>) {
        let phase = event.phase_string(&self.prefix);
        crate::ui::update_phase(&self.state, &phase);
    }
}

// ── ContextGatherer ─────────────────────────────────────────────────

/// Type-erased setter that moves a value into the accumulator struct.
type Setter<S> = Box<dyn FnOnce(Box<dyn Any + Send>, &mut S) + Send>;

/// A single named task in the gather pipeline.
struct GatherTask<S> {
    name: String,
    future: Pin<Box<dyn Future<Output = Box<dyn Any + Send>> + Send>>,
    setter: Setter<S>,
}

/// Declarative parallel context gatherer.
///
/// Collects heterogeneous async results into a caller-provided struct `S`.
/// Tasks are spawned onto the tokio runtime and collected as they complete.
/// A global deadline ensures the gather phase always terminates, abandoning
/// slow tasks and using whatever defaults `S` already contains.
///
/// Progress events are emitted through an optional [`GatherObserver`],
/// keeping the gatherer fully decoupled from any specific UI backend.
pub struct ContextGatherer<S> {
    tasks: Vec<GatherTask<S>>,
    deadline: Duration,
    default_task_timeout: Duration,
    observer: Option<Box<dyn GatherObserver>>,
}

impl<S: Send + 'static> ContextGatherer<S> {
    /// Create a new gatherer with the given global deadline.
    ///
    /// All tasks must complete within `deadline`, regardless of individual
    /// per-task timeouts.  Tasks that haven't finished by the deadline are
    /// abandoned and their setter is never called — the accumulator retains
    /// whatever default value it started with.
    pub fn new(deadline: Duration) -> Self {
        Self {
            tasks: Vec::new(),
            deadline,
            default_task_timeout: Duration::from_secs(45),
            observer: None,
        }
    }

    /// Set a default per-task timeout applied to tasks whose timeout is
    /// [`Duration::ZERO`].
    pub fn default_task_timeout(mut self, timeout: Duration) -> Self {
        self.default_task_timeout = timeout;
        self
    }

    /// Attach an observer for progress events.
    ///
    /// The observer receives [`GatherEvent`] variants as tasks start,
    /// complete, time out, or are abandoned.  Any UI backend can implement
    /// [`GatherObserver`] to render these events.
    ///
    /// For the common case of updating [`UiState`](crate::ui::UiState),
    /// use [`UiGatherObserver`]:
    ///
    /// ```ignore
    /// .observer(UiGatherObserver::new(ui_state, "Pre-computing context"))
    /// ```
    pub fn observer(mut self, obs: impl GatherObserver + 'static) -> Self {
        self.observer = Some(Box::new(obs));
        self
    }

    /// Register a task that produces a value of type `V`.
    ///
    /// - `name`: Human-readable label for logging and progress events.
    /// - `timeout`: Per-task timeout.  Pass [`Duration::ZERO`] to use the
    ///   default task timeout.
    /// - `future`: The async computation.
    /// - `setter`: Closure that stores the result into the accumulator.
    pub fn task<V, Fut, F>(mut self, name: &str, timeout: Duration, future: Fut, setter: F) -> Self
    where
        V: Send + 'static,
        Fut: Future<Output = V> + Send + 'static,
        F: FnOnce(V, &mut S) + Send + 'static,
    {
        let effective_timeout = if timeout.is_zero() {
            self.default_task_timeout
        } else {
            timeout
        };

        let name_owned = name.to_string();
        let name_for_timeout = name_owned.clone();

        // Wrap the future with a per-task timeout and type-erase the result.
        let wrapped = Box::pin(async move {
            match tokio::time::timeout(effective_timeout, future).await {
                Ok(value) => Box::new(Some(value)) as Box<dyn Any + Send>,
                Err(_) => {
                    warn!("{name_for_timeout}: timed out");
                    Box::new(None::<V>) as Box<dyn Any + Send>
                }
            }
        });

        // Type-erase the setter: downcast from Any back to Option<V>.
        let typed_setter: Setter<S> = Box::new(move |boxed: Box<dyn Any + Send>, state: &mut S| {
            if let Ok(opt) = boxed.downcast::<Option<V>>()
                && let Some(value) = *opt
            {
                setter(value, state);
            }
        });

        self.tasks.push(GatherTask {
            name: name_owned,
            future: wrapped,
            setter: typed_setter,
        });

        self
    }

    /// Execute all registered tasks in parallel, collecting results into `state`.
    ///
    /// Tasks are spawned onto the tokio runtime and collected as they finish.
    /// Progress events are emitted through the observer after each completion.
    /// When the global deadline is reached, remaining tasks are abandoned.
    pub async fn run(self, state: &mut S) {
        let Self {
            tasks,
            deadline,
            observer,
            ..
        } = self;

        let total = tasks.len();
        if total == 0 {
            return;
        }

        let task_names: Vec<String> = tasks.iter().map(|t| t.name.clone()).collect();
        let mut pending: HashSet<String> = task_names.iter().cloned().collect();

        emit(
            &observer,
            &GatherEvent::Started {
                total,
                tasks: &task_names,
            },
        );
        info!("Gathering context: starting {total} tasks");

        // Spawn all tasks, keeping the setter closures in a map keyed by
        // task index. The JoinSet yields (index, type-erased result) pairs.
        let mut setters: Vec<Option<Setter<S>>> = Vec::with_capacity(total);
        let mut js: JoinSet<(usize, Box<dyn Any + Send>)> = JoinSet::new();

        for (idx, task) in tasks.into_iter().enumerate() {
            setters.push(Some(task.setter));
            let future = task.future;
            js.spawn(async move { (idx, future.await) });
        }

        // Collect results as they arrive, up to the global deadline.
        let deadline_instant = tokio::time::Instant::now() + deadline;
        loop {
            let remaining = deadline_instant.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                emit_deadline(&observer, &pending);
                break;
            }

            match tokio::time::timeout(remaining, js.join_next()).await {
                // A task finished.
                Ok(Some(Ok((idx, result)))) => {
                    let name = &task_names[idx];
                    pending.remove(name);

                    let done = total - pending.len();
                    let pending_list: Vec<&str> = pending.iter().map(|s| s.as_str()).collect();

                    info!("Gathering context: {name} done ({done}/{total})");

                    // Call the setter to store the result.
                    if let Some(setter) = setters[idx].take() {
                        setter(result, state);
                    }

                    emit(
                        &observer,
                        &GatherEvent::TaskDone {
                            name,
                            pending: pending_list,
                            done,
                            total,
                        },
                    );
                }
                // A task panicked.
                Ok(Some(Err(e))) => {
                    warn!("Gathering context: task panicked: {e}");
                }
                // All tasks done.
                Ok(None) => break,
                // Timeout — deadline reached.
                Err(_) => {
                    emit_deadline(&observer, &pending);
                    break;
                }
            }
        }

        // Abort any still-running spawned tasks so they don't leak.
        js.abort_all();

        if pending.is_empty() {
            emit(&observer, &GatherEvent::Finished);
        }
    }
}

/// Emit an event to the observer (if present).
fn emit(observer: &Option<Box<dyn GatherObserver>>, event: &GatherEvent<'_>) {
    if let Some(obs) = observer {
        obs.on_gather_event(event);
    }
}

/// Emit a deadline event and log a warning about abandoned tasks.
fn emit_deadline(observer: &Option<Box<dyn GatherObserver>>, pending: &HashSet<String>) {
    if !pending.is_empty() {
        let abandoned: Vec<&str> = pending.iter().map(|s| s.as_str()).collect();
        warn!(
            "Gathering context: deadline reached — abandoning: {}",
            abandoned.join(", ")
        );
        emit(observer, &GatherEvent::Deadline { abandoned });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default, Debug)]
    struct TestCtx {
        a: String,
        b: i32,
        c: Vec<String>,
    }

    /// Test observer that records events for assertions.
    struct RecordingObserver {
        events: Mutex<Vec<String>>,
    }

    impl RecordingObserver {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
            }
        }
        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    impl GatherObserver for RecordingObserver {
        fn on_gather_event(&self, event: &GatherEvent<'_>) {
            let label = match event {
                GatherEvent::Started { total, .. } => format!("started:{total}"),
                GatherEvent::TaskDone { name, .. } => format!("done:{name}"),
                GatherEvent::TaskTimeout { name } => format!("timeout:{name}"),
                GatherEvent::Deadline { abandoned } => format!("deadline:{}", abandoned.join(",")),
                GatherEvent::Finished => "finished".into(),
            };
            self.events.lock().unwrap().push(label);
        }
    }

    #[tokio::test]
    async fn gather_collects_all_results() {
        let mut ctx = TestCtx::default();

        ContextGatherer::new(Duration::from_secs(5))
            .task(
                "string-task",
                Duration::from_secs(2),
                async { "hello".to_string() },
                |r, ctx: &mut TestCtx| ctx.a = r,
            )
            .task(
                "int-task",
                Duration::from_secs(2),
                async { 42i32 },
                |r, ctx: &mut TestCtx| ctx.b = r,
            )
            .task(
                "vec-task",
                Duration::from_secs(2),
                async { vec!["x".to_string(), "y".to_string()] },
                |r, ctx: &mut TestCtx| ctx.c = r,
            )
            .run(&mut ctx)
            .await;

        assert_eq!(ctx.a, "hello");
        assert_eq!(ctx.b, 42);
        assert_eq!(ctx.c, vec!["x", "y"]);
    }

    #[tokio::test]
    async fn gather_handles_task_timeout() {
        let mut ctx = TestCtx::default();
        ctx.a = "default".into();

        ContextGatherer::new(Duration::from_secs(5))
            .task(
                "fast",
                Duration::from_secs(2),
                async { 99i32 },
                |r, ctx: &mut TestCtx| ctx.b = r,
            )
            .task(
                "slow",
                Duration::from_millis(50),
                async {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    "should not arrive".to_string()
                },
                |r, ctx: &mut TestCtx| ctx.a = r,
            )
            .run(&mut ctx)
            .await;

        // Fast task succeeded, slow task timed out — default preserved.
        assert_eq!(ctx.b, 99);
        assert_eq!(ctx.a, "default");
    }

    #[tokio::test]
    async fn gather_handles_global_deadline() {
        let mut ctx = TestCtx::default();
        ctx.a = "untouched".into();

        ContextGatherer::new(Duration::from_millis(100))
            .task(
                "fast",
                Duration::from_secs(5),
                async { 7i32 },
                |r, ctx: &mut TestCtx| ctx.b = r,
            )
            .task(
                "very-slow",
                Duration::from_secs(60),
                async {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    "never".to_string()
                },
                |r, ctx: &mut TestCtx| ctx.a = r,
            )
            .run(&mut ctx)
            .await;

        // Fast task should still succeed within the deadline.
        assert_eq!(ctx.b, 7);
        // Slow task abandoned — default preserved.
        assert_eq!(ctx.a, "untouched");
    }

    #[tokio::test]
    async fn gather_empty_is_noop() {
        let mut ctx = TestCtx::default();
        ContextGatherer::<TestCtx>::new(Duration::from_secs(1))
            .run(&mut ctx)
            .await;
        assert_eq!(ctx.a, "");
        assert_eq!(ctx.b, 0);
    }

    #[tokio::test]
    async fn observer_receives_events() {
        let observer = Arc::new(RecordingObserver::new());
        let mut ctx = TestCtx::default();

        // We need to wrap the observer in an Arc-based adapter since
        // GatherObserver requires ownership.
        struct ArcObserver(Arc<RecordingObserver>);
        impl GatherObserver for ArcObserver {
            fn on_gather_event(&self, event: &GatherEvent<'_>) {
                self.0.on_gather_event(event);
            }
        }

        ContextGatherer::new(Duration::from_secs(5))
            .observer(ArcObserver(observer.clone()))
            .task(
                "alpha",
                Duration::from_secs(2),
                async { "a".to_string() },
                |r, ctx: &mut TestCtx| ctx.a = r,
            )
            .task(
                "beta",
                Duration::from_secs(2),
                async { 1i32 },
                |r, ctx: &mut TestCtx| ctx.b = r,
            )
            .run(&mut ctx)
            .await;

        let events = observer.events();
        assert_eq!(events[0], "started:2");
        // The two "done" events may arrive in either order.
        assert!(events.iter().any(|e| e == "done:alpha"));
        assert!(events.iter().any(|e| e == "done:beta"));
        assert_eq!(events.last().unwrap(), "finished");
    }

    #[tokio::test]
    async fn phase_string_formats_correctly() {
        let event = GatherEvent::TaskDone {
            name: "market",
            pending: vec!["repo-sync", "learnings"],
            done: 3,
            total: 5,
        };
        assert_eq!(
            event.phase_string("Gathering"),
            "Gathering (3/5) — waiting: repo-sync, learnings"
        );

        let done = GatherEvent::TaskDone {
            name: "last",
            pending: vec![],
            done: 5,
            total: 5,
        };
        assert_eq!(done.phase_string("Gathering"), "Gathering (done)");

        let finished = GatherEvent::Finished;
        assert_eq!(finished.phase_string("Ctx"), "Ctx (done)");
    }
}
