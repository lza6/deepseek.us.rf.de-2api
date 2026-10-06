//! 集成测试：用 mock 上游验证网关的 HTTP 层与协议翻译。
//!
//! 不依赖真实网络：启动一个本地 mock 上游 + mock 求解器，验证端到端 HTTP 行为。

use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

/// 启动 mock 上游（admin-ajax.php 行为）。
async fn spawn_mock_upstream() -> String {
    let app = Router::new()
        .route("/wp-admin/admin-ajax.php", post(mock_ajax).get(mock_sse))
        .route("/", get(mock_home))
        .with_state(Arc::new(()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

async fn mock_home() -> impl IntoResponse {
    "<div data-config='{\"botId\":27623,\"provider\":\"DeepSeek\"}'></div>"
}

#[derive(serde::Deserialize)]
struct Form {
    #[serde(default)]
    action: String,
    #[serde(default)]
    token: String,
}

async fn mock_ajax(axum::extract::Form(f): axum::extract::Form<Form>) -> impl IntoResponse {
    match f.action.as_str() {
        "deepseek_ts_verify" => {
            if f.token == "valid-token" {
                let mut h = axum::http::HeaderMap::new();
                h.insert(
                    "set-cookie",
                    axum::http::HeaderValue::from_static("dsts_ok=1; Path=/"),
                );
                (axum::http::StatusCode::OK, h, r#"{"ok":true}"#.to_string())
            } else {
                (
                    axum::http::StatusCode::FORBIDDEN,
                    axum::http::HeaderMap::new(),
                    r#"{"ok":false}"#.to_string(),
                )
            }
        }
        "aipkit_get_frontend_chat_nonce" => (
            axum::http::StatusCode::OK,
            axum::http::HeaderMap::new(),
            r#"{"success":true,"data":{"nonce":"testnonce"}}"#.to_string(),
        ),
        "aipkit_cache_sse_message" => (
            axum::http::StatusCode::OK,
            axum::http::HeaderMap::new(),
            r#"{"success":true,"data":{"cache_key":"aipkit_sse_testkey"}}"#.to_string(),
        ),
        _ => (
            axum::http::StatusCode::OK,
            axum::http::HeaderMap::new(),
            r#"{"success":false}"#.to_string(),
        ),
    }
}

async fn mock_sse(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    // 校验 cache_key
    if q.get("cache_key").map(|s| s.as_str()) != Some("aipkit_sse_testkey") {
        return Sse::new(futures::stream::iter(vec![Ok::<_, Infallible>(
            Event::default()
                .event("error")
                .data(r#"{"error":"Message not found in cache."}"#),
        )]));
    }
    let events = vec![
        Ok::<_, Infallible>(
            Event::default()
                .event("message_start")
                .data(r#"{"message_id":"aipkit-msg-test"}"#),
        ),
        Ok(Event::default().data(r#"{"delta":""}"#)),
        Ok(Event::default().data(r#"{"delta":"Hola"}"#)),
        Ok(Event::default().data(r#"{"delta":" mundo"}"#)),
        Ok(Event::default().event("done").data(r#"{"finished":true}"#)),
    ];
    Sse::new(futures::stream::iter(events))
}

/// 启动 mock 求解器。
async fn spawn_mock_solver() -> String {
    #[derive(serde::Deserialize)]
    struct P {
        #[serde(default)]
        #[allow(dead_code)]
        id: Option<String>,
    }
    let results: Arc<tokio::sync::Mutex<std::collections::HashMap<String, String>>> =
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let r1 = results.clone();
    let r2 = results.clone();
    let app = Router::new()
        .route(
            "/turnstile",
            get(move || {
                let rr = r1.clone();
                async move {
                    let task = "task-1".to_string();
                    rr.lock().await.insert(task.clone(), "pending".into());
                    Json(serde_json::json!({"task_id": task, "status": "accepted"}))
                }
            }),
        )
        .route(
            "/result",
            get(move |axum::extract::Query(_p): axum::extract::Query<P>| {
                let _rr = r2.clone();
                async move { Json(serde_json::json!({"status":"success","value":"valid-token"})) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

async fn spawn_gateway() -> String {
    let upstream = spawn_mock_upstream().await;
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        listen_addr: "127.0.0.1:0".into(),
        ..Default::default()
    };
    let client = Arc::new(deepseek_es_2api::UpstreamClient::new(cfg.clone()).unwrap());
    let state = Arc::new(deepseek_es_2api::api::AppState {
        cfg,
        upstream: client,
        sessions: deepseek_es_2api::session::SessionStore::new(Duration::from_secs(60)),
    });
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

async fn get_text(url: &str) -> (u16, String) {
    let r = reqwest::get(url).await.unwrap();
    let s = r.status().as_u16();
    (s, r.text().await.unwrap())
}

#[tokio::test]
async fn healthz_ok() {
    let base = spawn_gateway().await;
    let (s, body) = get_text(&format!("{base}/healthz")).await;
    assert_eq!(s, 200);
    assert!(body.contains("\"status\":\"ok\""));
}

#[tokio::test]
async fn models_listed() {
    let base = spawn_gateway().await;
    let (s, body) = get_text(&format!("{base}/v1/models")).await;
    assert_eq!(s, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["object"], "list");
    assert!(!v["data"].as_array().unwrap().is_empty());
    assert!(body.contains("deepseek-es"));
}

#[tokio::test]
async fn openai_stream_works() {
    let base = spawn_gateway().await;
    let client = reqwest::Client::new();
    let r = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "deepseek-es",
            "messages": [{"role":"user","content":"hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body = r.text().await.unwrap();
    // 应含 delta 内容与 DONE
    assert!(body.contains("Hola"), "body: {body}");
    assert!(body.contains(" mundo"), "body: {body}");
    assert!(body.contains("[DONE]"), "body: {body}");
    assert!(body.contains("chat.completion.chunk"), "body: {body}");
}

#[tokio::test]
async fn openai_nonstream_works() {
    let base = spawn_gateway().await;
    let client = reqwest::Client::new();
    let r = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "deepseek-es",
            "messages": [{"role":"user","content":"hi"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["object"], "chat.completion");
    assert_eq!(v["choices"][0]["message"]["content"], "Hola mundo");
    assert_eq!(v["choices"][0]["finish_reason"], "stop");
}

#[tokio::test]
async fn anthropic_stream_works() {
    let base = spawn_gateway().await;
    let client = reqwest::Client::new();
    let r = client
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model": "deepseek-es",
            "max_tokens": 100,
            "messages": [{"role":"user","content":"hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body = r.text().await.unwrap();
    assert!(body.contains("message_start"), "body: {body}");
    assert!(body.contains("content_block_delta"), "body: {body}");
    assert!(body.contains("message_stop"), "body: {body}");
    assert!(body.contains("Hola"), "body: {body}");
}

#[tokio::test]
async fn anthropic_nonstream_works() {
    let base = spawn_gateway().await;
    let client = reqwest::Client::new();
    let r = client
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model": "deepseek-es",
            "max_tokens": 100,
            "messages": [{"role":"user","content":"hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["type"], "message");
    assert_eq!(v["content"][0]["text"], "Hola mundo");
    assert_eq!(v["stop_reason"], "end_turn");
}

#[tokio::test]
async fn count_tokens_works() {
    let base = spawn_gateway().await;
    let client = reqwest::Client::new();
    let r = client
        .post(format!("{base}/v1/messages/count_tokens"))
        .json(&serde_json::json!({
            "model": "deepseek-es",
            "messages": [{"role":"user","content":"hello world"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    assert!(v["input_tokens"].as_u64().unwrap() >= 1);
}

#[tokio::test]
async fn bad_request_empty_messages() {
    let base = spawn_gateway().await;
    let client = reqwest::Client::new();
    let r = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "deepseek-es",
            "messages": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn auth_enforced_when_keys_configured() {
    // 单独构造一个带 key 的网关
    let upstream = spawn_mock_upstream().await;
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        api_keys: vec!["sk-secret".into()],
        ..Default::default()
    };
    let client = Arc::new(deepseek_es_2api::UpstreamClient::new(cfg.clone()).unwrap());
    let state = Arc::new(deepseek_es_2api::api::AppState {
        cfg,
        upstream: client,
        sessions: deepseek_es_2api::session::SessionStore::new(Duration::from_secs(60)),
    });
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);

    // 无 key → 401
    let r = reqwest::get(format!("{base}/v1/models")).await.unwrap();
    assert_eq!(r.status(), 401);

    // 正确 key → 200
    let r = reqwest::Client::new()
        .get(format!("{base}/v1/models"))
        .bearer_auth("sk-secret")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}
