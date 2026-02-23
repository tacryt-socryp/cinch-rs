//! Integration tests for the cinch-web server.
//!
//! These tests start a real axum server on a random port and exercise
//! the REST and WebSocket endpoints.

use std::sync::{Arc, Mutex};

use cinch_rs::ui::{
    ActiveQuestion, QuestionChoice, QuestionResponse, UiState, UserQuestion, push_agent_text,
    update_phase,
};
use cinch_web::{WebConfig, WsMessage, spawn_web};

/// Helper: spawn a test server on port 0 (random available port).
async fn spawn_test_server() -> (
    Arc<Mutex<UiState>>,
    String,
    tokio::sync::mpsc::Receiver<String>,
) {
    let state = Arc::new(Mutex::new(UiState::default()));
    let (tx, _) = tokio::sync::broadcast::channel::<WsMessage>(64);

    let config = WebConfig {
        bind_addr: ([127, 0, 0, 1], 0).into(),
        ..Default::default()
    };

    let (addr, chat_rx) = spawn_web(state.clone(), tx, config).await;
    let base = format!("http://{addr}");
    (state, base, chat_rx)
}

// ── REST Tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn get_state_returns_snapshot() {
    let (state, base, _chat_rx) = spawn_test_server().await;

    // Mutate the state so the snapshot has non-default values.
    update_phase(&state, "Testing");
    push_agent_text(&state, "Hello from test");

    let resp = reqwest::get(format!("{base}/api/state")).await.unwrap();
    assert_eq!(resp.status(), 200);

    let json: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(json["phase"], "Testing");
    assert!(json["running"].as_bool().unwrap());
    assert_eq!(json["agent_output"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn post_answer_sets_question_response() {
    let (state, base, _chat_rx) = spawn_test_server().await;

    // Set up an active question.
    {
        let mut s = state.lock().unwrap();
        s.active_question = Some(ActiveQuestion {
            question: UserQuestion {
                prompt: "Pick:".into(),
                choices: vec![
                    QuestionChoice {
                        label: "A".into(),
                        body: "Option A".into(),
                        metadata: String::new(),
                    },
                    QuestionChoice {
                        label: "B".into(),
                        body: "Option B".into(),
                        metadata: String::new(),
                    },
                ],
                editable: false,
                max_edit_length: None,
            },
            deadline: None,
            response: None,
            done: false,
        });
    }

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/answer"))
        .json(&serde_json::json!({"response": {"Selected": 1}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Verify the response was recorded.
    let s = state.lock().unwrap();
    let aq = s.active_question.as_ref().unwrap();
    assert!(aq.done);
    assert_eq!(aq.response, Some(QuestionResponse::Selected(1)));
}

#[tokio::test]
async fn post_answer_returns_404_when_no_question() {
    let (_state, base, _chat_rx) = spawn_test_server().await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/answer"))
        .json(&serde_json::json!({"response": "Skipped"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn post_control_quit() {
    let (state, base, _chat_rx) = spawn_test_server().await;

    assert!(!state.lock().unwrap().quit_requested);

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/control"))
        .json(&serde_json::json!({"action": "quit"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    assert!(state.lock().unwrap().quit_requested);
}

#[tokio::test]
async fn post_chat_delivers_message() {
    let (_state, base, mut chat_rx) = spawn_test_server().await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/chat"))
        .json(&serde_json::json!({"message": "Hello agent"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);

    // Verify the message was delivered to the receiver.
    let msg = chat_rx.try_recv().unwrap();
    assert_eq!(msg, "Hello agent");
}
