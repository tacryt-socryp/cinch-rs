//! Axum server setup and router construction.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::routing::{get, post};
use cinch_rs::ui::UiState;
use tokio::sync::{broadcast, mpsc};
use tower_http::cors::{Any, CorsLayer};
use tower_http::services::ServeDir;

use crate::api::{self, AppState};
use crate::broadcast::WsMessage;
use crate::ws::{self, WsState};

/// Build the full axum router.
///
/// The router serves:
/// - WebSocket at `/ws`
/// - REST API at `/api/*`
/// - Optional static files for the Next.js production build
pub fn build_router(
    ui_state: Arc<Mutex<UiState>>,
    broadcast_tx: broadcast::Sender<WsMessage>,
    chat_tx: mpsc::Sender<String>,
    static_dir: Option<PathBuf>,
) -> Router {
    let app_state = AppState {
        ui_state: ui_state.clone(),
        chat_tx: chat_tx.clone(),
        broadcast_tx: broadcast_tx.clone(),
    };

    let ws_state = WsState {
        ui_state,
        broadcast_tx,
        chat_tx,
    };

    // CORS layer for development (Next.js dev server on a different port).
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    // WebSocket routes (own state type).
    let ws_routes = Router::new()
        .route("/ws", get(ws::ws_upgrade))
        .with_state(ws_state);

    // REST API routes (own state type).
    let api_routes = Router::new()
        .route("/api/state", get(api::get_state))
        .route("/api/answer", post(api::post_answer))
        .route("/api/control", post(api::post_control))
        .route("/api/chat", post(api::post_chat))
        .with_state(app_state);

    // Merge into a single router.
    let mut router = Router::new().merge(ws_routes).merge(api_routes).layer(cors);

    // Serve static files (Next.js export) in production mode.
    if let Some(dir) = static_dir {
        router = router.fallback_service(ServeDir::new(dir));
    }

    router
}

/// Start the axum server and return the bound address.
pub async fn start_server(router: Router, bind_addr: SocketAddr) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind(bind_addr).await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    addr
}
