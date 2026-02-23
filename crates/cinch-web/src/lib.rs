//! Browser-based chat UI for cinch-rs powered agents.
//!
//! `cinch-web` provides an axum web server that exposes a WebSocket endpoint
//! for real-time agent observation and a REST API for control. It is designed
//! to be paired with a Next.js 16 frontend but works with any WebSocket client.
//!
//! # Quick start
//!
//! ```ignore
//! use cinch_web::{WebConfig, spawn_web};
//! use cinch_rs::ui::UiState;
//! use std::sync::{Arc, Mutex};
//!
//! let ui_state = Arc::new(Mutex::new(UiState::default()));
//! let (ws_tx, _) = tokio::sync::broadcast::channel(256);
//!
//! let config = WebConfig::default();
//! let (addr, chat_rx) = spawn_web(ui_state, ws_tx, config).await;
//! println!("Web UI: http://{addr}");
//!
//! // Read user messages sent from the browser:
//! while let Some(msg) = chat_rx.recv().await {
//!     println!("User said: {msg}");
//! }
//! ```
//!
//! # Architecture
//!
//! ```text
//! Agent runtime ──HarnessEvent──▶ WebBroadcastHandler ──WsMessage──▶ WebSocket clients
//!                                                                         ▲
//!           Arc<Mutex<UiState>> ◀── /api/answer, /api/control ────────────┘
//! ```
//!
//! The [`WebBroadcastHandler`] implements [`EventHandler`](cinch_rs::agent::events::EventHandler)
//! and converts harness events into serialized WebSocket messages. Compose it
//! alongside [`UiEventHandler`](cinch_rs::ui::event_handler::UiEventHandler)
//! in a [`CompositeEventHandler`](cinch_rs::agent::CompositeEventHandler).

mod api;
pub mod broadcast;
pub mod ext;
mod server;
pub mod snapshot;
mod ws;

pub use broadcast::{WebBroadcastHandler, WsMessage};
pub use ext::{ChoiceMetadata, NoWebExtension, StatusField, WebExtensionRenderer};
pub use snapshot::UiStateSnapshot;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cinch_rs::ui::UiState;

/// Configuration for the web server.
pub struct WebConfig {
    /// Address to bind to. Default: `127.0.0.1:3001`.
    pub bind_addr: SocketAddr,
    /// Path to the Next.js static export directory (for production mode).
    ///
    /// If `None`, only API/WS endpoints are served — the frontend runs
    /// separately (e.g., `next dev` on port 3000).
    pub static_dir: Option<PathBuf>,
    /// Maximum WebSocket broadcast channel capacity. Default: 256.
    ///
    /// Clients that fall behind by this many messages receive a fresh
    /// state snapshot to resynchronize.
    pub broadcast_capacity: usize,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            bind_addr: SocketAddr::from(([127, 0, 0, 1], 3001)),
            static_dir: None,
            broadcast_capacity: 256,
        }
    }
}

/// Spawn the web server on a Tokio task.
///
/// Returns the bound address and a receiver for chat messages sent from the
/// browser (via `POST /api/chat` or `{"type":"chat"}` WebSocket messages).
/// Read from the receiver in your agent loop to drive conversation turns.
///
/// The server runs until the Tokio runtime shuts down.
///
/// # Arguments
///
/// * `ui_state` — Shared agent state (same instance passed to `UiEventHandler`).
/// * `broadcast_tx` — Sender half of the WebSocket broadcast channel. Pass the
///   same sender to [`WebBroadcastHandler::new()`].
/// * `config` — Server configuration.
pub async fn spawn_web(
    ui_state: Arc<Mutex<UiState>>,
    broadcast_tx: tokio::sync::broadcast::Sender<WsMessage>,
    config: WebConfig,
) -> (SocketAddr, tokio::sync::mpsc::Receiver<String>) {
    let (chat_tx, chat_rx) = tokio::sync::mpsc::channel(32);
    let router = server::build_router(ui_state, broadcast_tx, chat_tx, config.static_dir);
    let addr = server::start_server(router, config.bind_addr).await;
    (addr, chat_rx)
}
