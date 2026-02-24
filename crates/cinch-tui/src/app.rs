//! TUI-local state (not shared with the agent).

/// Input mode for the TUI.
pub(crate) enum InputMode {
    /// Normal mode — arrow keys scroll, `q` quits.
    Normal,
    /// Question selection mode — arrow keys navigate choices, Enter selects.
    QuestionSelect,
    /// Question editing mode — the user is editing a selected choice before confirming.
    /// Pre-filled with the original text; Enter confirms, Esc cancels back to select.
    QuestionEdit,
    /// Free-text input mode — user types a prompt, Enter submits, Esc cancels.
    FreeText,
}

/// Which pane currently receives scroll input.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActivePane {
    Log,
    AgentOutput,
}

/// TUI-local state (not shared with the agent).
pub(crate) struct App {
    pub(crate) input_mode: InputMode,
    pub(crate) input_buffer: String,
    /// Which pane is focused for scrolling (toggled with Tab).
    pub(crate) active_pane: ActivePane,
    /// Whether the logs pane is visible (toggled with `,`).
    pub(crate) show_logs: bool,
    /// Offset from the bottom of the log (0 = follow tail).
    pub(crate) log_scroll: usize,
    /// Offset from the bottom of the agent output (0 = follow tail).
    pub(crate) agent_scroll: usize,
    /// Status messages shown temporarily at the bottom.
    pub(crate) status_message: Option<String>,
    pub(crate) should_quit: bool,
    /// Currently highlighted choice index in question-select mode.
    pub(crate) question_cursor: usize,
    /// Scroll offset for the question choice list (top visible index).
    pub(crate) question_scroll: usize,
    /// True when the agent is actively running (not waiting for input).
    /// Used to show the interrupt hint in the status bar.
    pub(crate) agent_busy: bool,
}

impl App {
    pub(crate) fn new() -> Self {
        Self {
            input_mode: InputMode::Normal,
            input_buffer: String::new(),
            active_pane: ActivePane::AgentOutput,
            show_logs: false,
            log_scroll: 0,
            agent_scroll: 0,
            status_message: None,
            should_quit: false,
            question_cursor: 0,
            question_scroll: 0,
            agent_busy: false,
        }
    }
}
