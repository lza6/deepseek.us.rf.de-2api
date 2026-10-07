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

async fn mock_home(headers: axum::http::HeaderMap) -> impl IntoResponse {
    // dsgtConfig 的 restUrl 用请求的 Host 拼出（供 /v1/balance 解析）。
    let host = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1");
    let url = format!("http://{host}/wp-json/dsgt/v1/");
    format!(
        "<div data-config='{{\"botId\":27623,\"provider\":\"DeepSeek\"}}'></div>\
         <script>var dsgtConfig = {{\"restUrl\":\"{url}\",\"nonce\":\"testnonce\"}};</script>"
    )
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

/// 首次 cache_key（k0）→ ts_required；之后 → 正常流（验证自愈重试）。
async fn spawn_mock_upstream_ts_required() -> String {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let c1 = count.clone();
    let app = Router::new().route("/", get(mock_home)).route(
        "/wp-admin/admin-ajax.php",
        post(move |f: axum::extract::Form<Form>| {
            let c = c1.clone();
            async move {
                match f.action.as_str() {
                    "deepseek_ts_verify" => (
                        axum::http::StatusCode::OK,
                        axum::http::HeaderMap::new(),
                        r#"{"ok":true}"#.to_string(),
                    ),
                    "aipkit_get_frontend_chat_nonce" => (
                        axum::http::StatusCode::OK,
                        axum::http::HeaderMap::new(),
                        r#"{"success":true,"data":{"nonce":"n"}}"#.to_string(),
                    ),
                    "aipkit_cache_sse_message" => {
                        let n = c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        (
                            axum::http::StatusCode::OK,
                            axum::http::HeaderMap::new(),
                            format!(r#"{{"success":true,"data":{{"cache_key":"k{n}"}}}}"#),
                        )
                    }
                    _ => (
                        axum::http::StatusCode::OK,
                        axum::http::HeaderMap::new(),
                        r#"{"success":false}"#.to_string(),
                    ),
                }
            }
        })
        .get(mock_sse_ts_required),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

/// cache_key=k0 → ts_required；其余 → 正常文本流。
async fn mock_sse_ts_required(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    if q.get("cache_key").map(|s| s.as_str()) == Some("k0") {
        let events = vec![
            Ok::<_, Infallible>(
                Event::default()
                    .event("error")
                    .data(r#"{"error":"Sicherheitspruefung erforderlich.","ts_required":true}"#),
            ),
            Ok(Event::default().event("done").data(r#"{"finished":true}"#)),
        ];
        return Sse::new(futures::stream::iter(events));
    }
    let events = vec![
        Ok::<_, Infallible>(
            Event::default()
                .event("message_start")
                .data(r#"{"message_id":"m1"}"#),
        ),
        Ok(Event::default().data(r#"{"delta":"recuperado"}"#)),
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

/// 构造测试用 AppState（含新增的 cache/ledger/replay 字段）。
///
/// 账本路径：测试间必须**隔离**——默认 `usage.db` 会让并行测试争抢同一文件
/// （导致 "database is locked"）。此处强制改用进程内唯一临时文件。
fn make_state(cfg: deepseek_es_2api::Config) -> Arc<deepseek_es_2api::api::AppState> {
    let client = Arc::new(deepseek_es_2api::UpstreamClient::new(cfg.clone()).unwrap());
    let ledger_path = if cfg.ledger_path == "usage.db" {
        // 未显式指定 → 用唯一临时路径，避免测试互相干扰
        let p = std::env::temp_dir().join(format!(
            "dses2api-test-{}-{:x}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        p.to_string_lossy().to_string()
    } else {
        cfg.ledger_path.clone()
    };
    Arc::new(deepseek_es_2api::api::AppState {
        cfg: cfg.clone(),
        upstream: client,
        sessions: deepseek_es_2api::session::SessionStore::new(Duration::from_secs(60)),
        cache: deepseek_es_2api::cache::ResponseCache::new(
            cfg.cache_ttl_secs,
            cfg.cache_max_entries,
            cfg.cache_min_chars,
        ),
        ledger: deepseek_es_2api::ledger::Ledger::open(&ledger_path).unwrap(),
        replay: deepseek_es_2api::replay::ReplayStore::new(60, 100, 1000),
    })
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
    let state = make_state(cfg);
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
async fn ts_required_triggers_reauth_and_retry() {
    // 首次 SSE 返回 ts_required，第二次返回正常文本：验证自愈重试。
    let upstream = spawn_mock_upstream_ts_required().await;
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        listen_addr: "127.0.0.1:0".into(),
        ..Default::default()
    };
    let state = make_state(cfg);
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);

    // 非流式：应自动重认证并返回完整回复，而非 502
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "deepseek-es",
            "messages": [{"role":"user","content":"hola"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "自愈后应 200");
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "recuperado");
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
    let state = make_state(cfg);
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

// ── 通用网关启动（可改配置）───────────────────────────────

async fn serve_gateway(
    upstream_url: String,
    solver_url: String,
    mutate: impl FnOnce(&mut deepseek_es_2api::Config),
) -> String {
    let mut cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream_url,
        cf_solver_url: solver_url,
        solver_timeout_secs: 10,
        listen_addr: "127.0.0.1:0".into(),
        ..Default::default()
    };
    mutate(&mut cfg);
    let state = make_state(cfg);
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

/// mock 上游：SSE 首个事件为配额耗尽（quota_notice）。
async fn spawn_mock_upstream_quota() -> String {
    let app = Router::new().route("/", get(mock_home)).route(
        "/wp-admin/admin-ajax.php",
        post(mock_ajax).get(mock_sse_quota),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

async fn mock_sse_quota(
    _q: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let events = vec![Ok::<_, Infallible>(Event::default().event("error").data(
        r#"{"error":"Cuota diaria agotada","quota_notice":{"title":"t","message":"m"}}"#,
    ))];
    Sse::new(futures::stream::iter(events))
}

/// 一直失败的求解器。
async fn spawn_mock_solver_fail() -> String {
    let app = Router::new()
        .route(
            "/turnstile",
            get(|| async { Json(serde_json::json!({"task_id":"t","status":"accepted"})) }),
        )
        .route(
            "/result",
            get(|| async { Json(serde_json::json!({"status":"error","message":"boom"})) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

/// 首次求解失败、之后成功的求解器。
async fn spawn_mock_solver_flaky_once() -> String {
    let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let n1 = n.clone();
    let app = Router::new()
        .route(
            "/turnstile",
            get(move || {
                let n = n1.clone();
                async move {
                    let id = n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Json(serde_json::json!({"task_id": format!("t{id}"), "status":"accepted"}))
                }
            }),
        )
        .route(
            "/result",
            get(
                |axum::extract::Query(q): axum::extract::Query<
                    std::collections::HashMap<String, String>,
                >| async move {
                    let id = q.get("id").cloned().unwrap_or_default();
                    if id == "t0" {
                        Json(serde_json::json!({"status":"error","message":"boom"}))
                    } else {
                        Json(serde_json::json!({"status":"success","value":"valid-token"}))
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

// ── P1-3：配额 → 429 ─────────────────────────────────────

#[tokio::test]
async fn quota_exhausted_returns_429() {
    let base = serve_gateway(
        spawn_mock_upstream_quota().await,
        spawn_mock_solver().await,
        |_| {},
    )
    .await;
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "deepseek-es",
            "messages": [{"role":"user","content":"hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429, "配额耗尽应映射 429");
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["error"]["type"], "rate_limit_error");
}

/// M10：**流式**请求的配额耗尽也必须返回 HTTP 429（而非 200 + 错误帧）。
#[tokio::test]
async fn streaming_quota_exhausted_returns_429() {
    let base = serve_gateway(
        spawn_mock_upstream_quota().await,
        spawn_mock_solver().await,
        |_| {},
    )
    .await;
    // OpenAI 流式
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "deepseek-es",
            "messages": [{"role":"user","content":"hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        429,
        "流式配额耗尽应返回 429，实际 {}",
        r.status()
    );
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["error"]["type"], "rate_limit_error", "{v}");

    // Anthropic 流式
    let r2 = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model": "deepseek-es", "max_tokens": 100,
            "messages": [{"role":"user","content":"hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 429, "Anthropic 流式配额耗尽应 429");
    // H3：Anthropic 错误体结构
    let v2: serde_json::Value = r2.json().await.unwrap();
    assert_eq!(v2["type"], "error", "{v2}");
    assert_eq!(v2["error"]["type"], "rate_limit_error", "{v2}");
}

// ── P1-5：求解重试 ───────────────────────────────────────

#[tokio::test]
async fn solver_retry_recovers_from_transient_failure() {
    let base = serve_gateway(
        spawn_mock_upstream().await,
        spawn_mock_solver_flaky_once().await,
        |c| c.solver_retries = 2,
    )
    .await;
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "deepseek-es",
            "messages": [{"role":"user","content":"hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "求解首次失败后重试应成功");
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "Hola mundo");
}

// ── P1-5：熔断 ───────────────────────────────────────────

#[tokio::test]
async fn circuit_breaker_opens_after_failures() {
    let base = serve_gateway(
        spawn_mock_upstream().await,
        spawn_mock_solver_fail().await,
        |c| {
            c.solver_retries = 0;
            c.breaker_fail_threshold = 1;
            c.breaker_cooldown_secs = 60;
        },
    )
    .await;
    let client = reqwest::Client::new();
    let body = |b: &str| b.to_string();
    // 第一次：真实求解失败 → 熔断打开
    let b1 = client
        .post(format!("{base}/v1/chat/completions"))
        .json(
            &serde_json::json!({"model":"deepseek-es","messages":[{"role":"user","content":"hi"}]}),
        )
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        body(&b1).contains("求解") || body(&b1).contains("失败"),
        "b1={b1}"
    );
    // 第二次：熔断 → 快速失败且提示熔断
    let b2 = client
        .post(format!("{base}/v1/chat/completions"))
        .json(
            &serde_json::json!({"model":"deepseek-es","messages":[{"role":"user","content":"hi"}]}),
        )
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body(&b2).contains("熔断"), "第二次应被熔断: {b2}");
}

// ── P1-5：限流 ───────────────────────────────────────────

#[tokio::test]
async fn rate_limit_returns_429() {
    let base = serve_gateway(
        spawn_mock_upstream().await,
        spawn_mock_solver().await,
        |c| c.rate_limit_per_sec = 2,
    )
    .await;
    let client = reqwest::Client::new();
    let mut got_429 = 0;
    for _ in 0..10 {
        let r = client
            .get(format!("{base}/v1/models"))
            .send()
            .await
            .unwrap();
        if r.status() == 429 {
            got_429 += 1;
        }
    }
    assert!(got_429 >= 1, "突发请求应触发限流 (429 计数={got_429})");
}

// ── P1-2：模型诚实化 ─────────────────────────────────────

#[tokio::test]
async fn models_single_routable() {
    let base = spawn_gateway().await;
    let v: serde_json::Value = reqwest::get(format!("{base}/v1/models"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let data = v["data"].as_array().unwrap();
    assert_eq!(data.len(), 1, "应仅暴露单一真实模型");
    assert_eq!(data[0]["id"], "deepseek-es");
    assert_eq!(data[0]["routable"], true);
    assert!(data[0]["alias_of"].is_null());
}

// ══ P3 增强功能集成测试 ═════════════════════════════════

/// P3-2：响应缓存——相同无会话请求第二次不打上游。
#[tokio::test]
async fn response_cache_hits_on_repeat() {
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h = hits.clone();
    let app = Router::new().route("/", get(mock_home)).route(
        "/wp-admin/admin-ajax.php",
        post(move |f: axum::extract::Form<Form>| {
            let h = h.clone();
            async move {
                match f.action.as_str() {
                    "deepseek_ts_verify" => {
                        let mut hd = axum::http::HeaderMap::new();
                        hd.insert("set-cookie", "dsts_ok=1; Path=/".parse().unwrap());
                        (axum::http::StatusCode::OK, hd, r#"{"ok":true}"#.to_string())
                    }
                    "aipkit_get_frontend_chat_nonce" => (
                        axum::http::StatusCode::OK,
                        axum::http::HeaderMap::new(),
                        r#"{"success":true,"data":{"nonce":"n"}}"#.to_string(),
                    ),
                    "aipkit_cache_sse_message" => {
                        h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        (
                            axum::http::StatusCode::OK,
                            axum::http::HeaderMap::new(),
                            r#"{"success":true,"data":{"cache_key":"aipkit_sse_testkey"}}"#
                                .to_string(),
                        )
                    }
                    _ => (
                        axum::http::StatusCode::OK,
                        axum::http::HeaderMap::new(),
                        r#"{"success":false}"#.to_string(),
                    ),
                }
            }
        })
        .get(mock_sse),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let upstream = format!("http://{}", addr);
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        cache_ttl_secs: 60,
        ledger_path: String::new(),
        ..Default::default()
    };
    let app = deepseek_es_2api::api::build_router(make_state(cfg));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": "deepseek-es",
        "messages": [{"role":"user","content":"cache me"}],
        "stream": false
    });
    let r1 = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r1.status(), 200);
    let after_first = hits.load(std::sync::atomic::Ordering::SeqCst);
    let r2 = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 200);
    let after_second = hits.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        after_first, after_second,
        "第二次应命中缓存、不再打上游 (first={after_first} second={after_second})"
    );
    let v: serde_json::Value = r2.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "Hola mundo");
}

/// P3-1：控制台——未启用 404；启用后无/错令牌 401；正确令牌 200。
#[tokio::test]
async fn admin_console_auth() {
    let base = spawn_gateway().await;
    let r = reqwest::get(format!("{base}/admin")).await.unwrap();
    assert_eq!(r.status(), 404, "未启用控制台应 404");

    let upstream = spawn_mock_upstream().await;
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        admin_enabled: true,
        admin_token: "s3cret".into(),
        ledger_path: String::new(),
        ..Default::default()
    };
    let app = deepseek_es_2api::api::build_router(make_state(cfg));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let abase = format!("http://{}", addr);

    assert_eq!(
        reqwest::get(format!("{abase}/admin"))
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        reqwest::get(format!("{abase}/admin?token=wrong"))
            .await
            .unwrap()
            .status(),
        401
    );
    let r = reqwest::get(format!("{abase}/admin?token=s3cret"))
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.text().await.unwrap().contains("控制台"));
    let r = reqwest::get(format!("{abase}/admin/api/status?token=s3cret"))
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    assert!(v["models"].is_array());
    assert_eq!(v["solver"].as_array().unwrap().len(), 1);
}

/// P3-3：账本——请求后统计可查。
#[tokio::test]
async fn ledger_records_usage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("u.db");
    let upstream = spawn_mock_upstream().await;
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        ledger_path: path.to_str().unwrap().to_string(),
        ..Default::default()
    };
    let state = make_state(cfg);
    let ledger = state.ledger.clone();
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es",
            "messages":[{"role":"user","content":"hi"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let s = ledger.stats(None).await.unwrap();
    assert_eq!(s.total_requests, 1, "应记录 1 条用量");
    assert!(s.total_prompt_tokens > 0);
}

/// P3-7：伪工具——模型输出含 tool 块时，网关本地执行并回填。
#[tokio::test]
async fn pseudo_tool_executed() {
    let app = Router::new()
        .route("/", get(mock_home))
        .route("/wp-admin/admin-ajax.php", post(mock_ajax).get(tool_sse));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let upstream = format!("http://{}", addr);
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        ledger_path: String::new(),
        ..Default::default()
    };
    let app = deepseek_es_2api::api::build_router(make_state(cfg));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es",
            "messages":[{"role":"user","content":"time?"}],
            "stream": false
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let content = v["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(
        content.contains("[tool:get_time]"),
        "应回填工具执行结果: {content}"
    );
}

/// P3-5：断线重放——网关为每个 SSE 事件带 id: 递增序号。
#[tokio::test]
async fn sse_events_have_ids() {
    let base = spawn_gateway().await;
    let body = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es",
            "messages":[{"role":"user","content":"hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("id: 1"), "首事件应带 id: 1\n{body}");
    assert!(body.contains("id: 2"), "次事件应带 id: 2\n{body}");
}

async fn tool_sse(
    _q: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let delta = r#"{"delta":"check\n```tool\n{\"name\":\"get_time\",\"arguments\":{}}\n```"}"#;
    let events = vec![
        Ok::<_, Infallible>(
            Event::default()
                .event("message_start")
                .data(r#"{"message_id":"m"}"#),
        ),
        Ok(Event::default().data(delta)),
        Ok(Event::default().event("done").data(r#"{"finished":true}"#)),
    ];
    Sse::new(futures::stream::iter(events))
}

// ══ 审计修复验证（重放可达 + 全路径入账）═════════════════

/// HIGH：断线重放端点必须**可达**（此前 replay() 是死代码）。
#[tokio::test]
async fn replay_endpoint_is_reachable() {
    let base = spawn_gateway().await;
    // 发起一次流式请求，取 x-response-id
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es",
            "messages":[{"role":"user","content":"hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    let rid = resp
        .headers()
        .get("x-response-id")
        .expect("流式响应应带 x-response-id")
        .to_str()
        .unwrap()
        .to_string();
    let body = resp.text().await.unwrap();
    assert!(body.contains("id: 1"), "应带递增 id\n{body}");

    // 用该 id 调重放端点：从头回放（after=0）
    let r = reqwest::get(format!("{base}/v1/responses/{rid}?after=0"))
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "重放端点应可达");
    let rb = r.text().await.unwrap();
    assert!(rb.contains("id: 1"), "应回放事件\n{rb}");

    // 带 Last-Event-ID 头回放后半段
    let r2 = reqwest::Client::new()
        .get(format!("{base}/v1/responses/{rid}"))
        .header("last-event-id", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 200);
    let rb2 = r2.text().await.unwrap();
    assert!(!rb2.contains("id: 1\n"), "应跳过 seq<=1\n{rb2}");

    // 未知 id → 409
    let r3 = reqwest::get(format!("{base}/v1/responses/nonexistent-id?after=0"))
        .await
        .unwrap();
    assert_eq!(r3.status(), 409, "未知响应应 409");
}

/// HIGH：流式请求也必须入账（此前仅非流式被记录）。
#[tokio::test]
async fn streaming_requests_are_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    let upstream = spawn_mock_upstream().await;
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        ledger_path: path.to_str().unwrap().to_string(),
        ..Default::default()
    };
    let state = make_state(cfg);
    let ledger = state.ledger.clone();
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);
    let _ = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es",
            "messages":[{"role":"user","content":"hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let st = ledger.stats(None).await.unwrap();
    assert_eq!(st.total_requests, 1, "流式请求应入账");
}

/// HIGH：Anthropic 请求也必须入账。
#[tokio::test]
async fn anthropic_requests_are_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.db");
    let upstream = spawn_mock_upstream().await;
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        ledger_path: path.to_str().unwrap().to_string(),
        ..Default::default()
    };
    let state = make_state(cfg);
    let ledger = state.ledger.clone();
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);
    let _ = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model":"deepseek-es",
            "max_tokens": 50,
            "messages":[{"role":"user","content":"hi"}]
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let st = ledger.stats(None).await.unwrap();
    assert_eq!(st.total_requests, 1, "Anthropic 请求应入账");
}

// ── H2 回归：多轮历史真实送达上游 ─────────────────────────────

/// mock 上游：记录收到的 `message` 表单字段（即网关送给上游的真实 prompt）。
async fn spawn_mock_upstream_recording() -> (String, Arc<tokio::sync::Mutex<Vec<String>>>) {
    let seen: Arc<tokio::sync::Mutex<Vec<String>>> = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let s1 = seen.clone();

    #[derive(serde::Deserialize)]
    struct F2 {
        #[serde(default)]
        action: String,
        #[serde(default)]
        message: String,
    }

    let app = Router::new()
        .route("/", get(mock_home))
        .route(
            "/wp-admin/admin-ajax.php",
            post(move |axum::extract::Form(f): axum::extract::Form<F2>| {
                let s = s1.clone();
                async move {
                    match f.action.as_str() {
                        "deepseek_ts_verify" => {
                            let mut h = axum::http::HeaderMap::new();
                            h.insert(
                                "set-cookie",
                                axum::http::HeaderValue::from_static("dsts_ok=1; Path=/"),
                            );
                            (axum::http::StatusCode::OK, h, r#"{"ok":true}"#.to_string())
                        }
                        "aipkit_get_frontend_chat_nonce" => (
                            axum::http::StatusCode::OK,
                            axum::http::HeaderMap::new(),
                            r#"{"success":true,"data":{"nonce":"n"}}"#.to_string(),
                        ),
                        "aipkit_cache_sse_message" => {
                            s.lock().await.push(f.message.clone());
                            (
                                axum::http::StatusCode::OK,
                                axum::http::HeaderMap::new(),
                                r#"{"success":true,"data":{"cache_key":"aipkit_sse_testkey"}}"#
                                    .to_string(),
                            )
                        }
                        _ => (
                            axum::http::StatusCode::OK,
                            axum::http::HeaderMap::new(),
                            r#"{"success":false}"#.to_string(),
                        ),
                    }
                }
            }),
        )
        .route("/wp-admin/admin-ajax.php", get(mock_sse));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("http://{}", addr), seen)
}

/// HIGH：多轮对话历史必须被完整送入上游（否则模型只看到最后一句话）。
#[tokio::test]
async fn multi_turn_history_reaches_upstream() {
    let (upstream, seen) = spawn_mock_upstream_recording().await;
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        ..Default::default()
    };
    let state = make_state(cfg);
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);

    let _ = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es",
            "messages":[
                {"role":"user","content":"我叫小明"},
                {"role":"assistant","content":"你好小明"},
                {"role":"user","content":"我叫什么"}
            ]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    let prompts = seen.lock().await;
    assert!(!prompts.is_empty(), "上游未收到任何 message");
    let p = &prompts[0];
    assert!(p.contains("我叫小明"), "首轮历史未送上游: {p}");
    assert!(p.contains("你好小明"), "assistant 轮未送上游: {p}");
    assert!(p.contains("我叫什么"), "末轮未送上游: {p}");
}

/// HIGH：无 `user` 字段的**单轮**请求保持无状态（不引入随机串扰），
/// 多轮请求则自带完整历史，两者都不依赖上游 conv_uuid。
#[tokio::test]
async fn multibyte_content_survives_gateway() {
    // H1 端到端：mock 上游发多字节内容，网关输出不得出现 U+FFFD。
    let upstream = spawn_mock_upstream_multibyte().await;
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        ..Default::default()
    };
    let state = make_state(cfg);
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);

    let body = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es",
            "messages":[{"role":"user","content":"hi"}]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("你好世界"), "多字节内容丢失: {body}");
    assert!(!body.contains('\u{FFFD}'), "出现替换符: {body}");
}

/// mock 上游：SSE 含中文内容（验证 H1 端到端）。
async fn spawn_mock_upstream_multibyte() -> String {
    let app = Router::new()
        .route("/", get(mock_home))
        .route("/wp-admin/admin-ajax.php", post(mock_ajax))
        .route("/wp-admin/admin-ajax.php", get(mock_sse_multibyte));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

async fn mock_sse_multibyte(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
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
                .data(r#"{"message_id":"m"}"#),
        ),
        Ok(Event::default().data(r#"{"delta":"你好"}"#)),
        Ok(Event::default().data(r#"{"delta":"世界"}"#)),
        Ok(Event::default().event("done").data(r#"{"finished":true}"#)),
    ];
    Sse::new(futures::stream::iter(events))
}

// ── H3 回归：Anthropic 端点错误体必须是 Anthropic 结构 ────────────

/// Anthropic 端点鉴权失败 → 错误体应为 `{"type":"error","error":{...}}`（非 OpenAI 结构）。
#[tokio::test]
async fn anthropic_error_body_is_anthropic_shaped() {
    let upstream = spawn_mock_upstream().await;
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        api_keys: vec!["sk-secret".into()],
        ..Default::default()
    };
    let state = make_state(cfg);
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);

    // 不带 key → 401
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model":"deepseek-es","max_tokens":10,
            "messages":[{"role":"user","content":"hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["type"], "error", "缺少顶层 type:error: {v}");
    assert!(v["error"]["type"].is_string(), "error.type 缺失: {v}");
    assert!(v["error"]["message"].is_string(), "error.message 缺失: {v}");
}

/// Anthropic 端点参数错误（缺 messages）→ 也是 Anthropic 结构。
#[tokio::test]
async fn anthropic_bad_request_is_anthropic_shaped() {
    let base = spawn_gateway().await;
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({ "model": "deepseek-es", "max_tokens": 10, "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["type"], "error", "缺少顶层 type:error: {v}");
    assert_eq!(v["error"]["type"], "invalid_request_error", "{v}");
}

/// OpenAI 端点错误体保持 OpenAI 结构（回归：H3 不得影响 OpenAI）。
#[tokio::test]
async fn openai_error_body_stays_openai_shaped() {
    let base = spawn_gateway().await;
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({ "model": "deepseek-es", "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let v: serde_json::Value = r.json().await.unwrap();
    // OpenAI 结构：顶层 error，无 type:error
    assert!(v["error"].is_object(), "OpenAI 错误结构被破坏: {v}");
    assert!(v.get("type").is_none(), "OpenAI 错误不应有顶层 type: {v}");
}

// ── M4 回归：Anthropic 流式 usage 必须为真实值（非 0/0） ─────────

#[tokio::test]
async fn anthropic_stream_usage_is_nonzero() {
    let base = spawn_gateway().await;
    let body = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model": "deepseek-es", "max_tokens": 100,
            "messages": [{"role":"user","content":"hi"}], "stream": true
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    // message_delta 携带 usage.output_tokens，必须 > 0
    let md_line = body
        .lines()
        .zip(body.lines().skip(1))
        .find(|(a, _)| a.contains("message_delta"))
        .map(|(_, b)| b.to_string())
        .expect("未找到 message_delta");
    let json = md_line.trim_start_matches("data:").trim();
    let v: serde_json::Value = serde_json::from_str(json).unwrap();
    let out = v["usage"]["output_tokens"].as_u64().unwrap_or(0);
    assert!(out > 0, "message_delta usage.output_tokens 仍为 0: {v}");
}

// ── M7 回归：上游异常结束（无 done）必须补全结束序列 ─────────────

async fn spawn_mock_upstream_truncated() -> String {
    let app = Router::new()
        .route("/", get(mock_home))
        .route("/wp-admin/admin-ajax.php", post(mock_ajax))
        .route("/wp-admin/admin-ajax.php", get(mock_sse_truncated));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

/// 上游只发 delta、**不发 done** 就结束（模拟中途断开）。
async fn mock_sse_truncated(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
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
                .data(r#"{"message_id":"m"}"#),
        ),
        Ok(Event::default().data(r#"{"delta":"partial"}"#)),
        // 无 done 事件
    ];
    Sse::new(futures::stream::iter(events))
}

async fn spawn_gateway_with(upstream: String) -> String {
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        ..Default::default()
    };
    let state = make_state(cfg);
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

#[tokio::test]
async fn openai_stream_abrupt_close_has_finish_reason() {
    let base = spawn_gateway_with(spawn_mock_upstream_truncated().await).await;
    let body = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es","messages":[{"role":"user","content":"hi"}],"stream":true
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("partial"), "内容未送达: {body}");
    assert!(
        body.contains("finish_reason"),
        "异常结束缺少 finish_reason: {body}"
    );
    assert!(body.contains("[DONE]"), "缺少 [DONE]: {body}");
}

#[tokio::test]
async fn anthropic_stream_abrupt_close_has_message_stop() {
    let base = spawn_gateway_with(spawn_mock_upstream_truncated().await).await;
    let body = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model":"deepseek-es","max_tokens":100,
            "messages":[{"role":"user","content":"hi"}],"stream":true
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("partial"), "内容未送达: {body}");
    assert!(
        body.contains("message_stop"),
        "异常结束缺少 message_stop: {body}"
    );
    assert!(
        body.contains("message_delta"),
        "异常结束缺少 message_delta: {body}"
    );
}

// ── M11：Anthropic 非流式响应上限 ───────────────────────────────

#[tokio::test]
async fn anthropic_nonstream_respects_max_response_bytes() {
    // 上限设为 1 字节，mock 上游必然超过 → 应返回错误而非无界聚合
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: spawn_mock_upstream().await,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        max_response_bytes: 1,
        ..Default::default()
    };
    let state = make_state(cfg);
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model":"deepseek-es","max_tokens":100,
            "messages":[{"role":"user","content":"hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(
        !r.status().is_success(),
        "超限应返回错误，实际 {}",
        r.status()
    );
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["type"], "error", "错误体应为 Anthropic 结构: {v}");
}

// ── H4：伪工具说明注入（端到端断言 prompt 含工具说明） ───────────

#[tokio::test]
async fn tool_instruction_reaches_upstream_when_enabled() {
    let (upstream, seen) = spawn_mock_upstream_recording().await;
    let solver = spawn_mock_solver().await;
    let cfg = deepseek_es_2api::Config {
        upstream_base_url: upstream,
        cf_solver_url: solver,
        solver_timeout_secs: 10,
        pseudo_tools_enabled: true,
        ..Default::default()
    };
    let state = make_state(cfg);
    let app = deepseek_es_2api::api::build_router(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let base = format!("http://{}", addr);
    let _ = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es","messages":[{"role":"user","content":"现在几点"}]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let prompts = seen.lock().await;
    assert!(!prompts.is_empty(), "上游未收到 message");
    assert!(
        prompts[0].contains("get_time"),
        "工具说明未送达上游: {}",
        prompts[0]
    );
    assert!(
        prompts[0].contains("```tool"),
        "工具块说明未送达: {}",
        prompts[0]
    );
}

// ── v2.0.0：协议级工具调用（端到端） ───────────────────────────

/// mock 上游：SSE 输出一段文本 + 一个 ```tool 块。
async fn spawn_mock_upstream_toolcall() -> String {
    let app = Router::new()
        .route("/", get(mock_home))
        .route("/wp-admin/admin-ajax.php", post(mock_ajax))
        .route("/wp-admin/admin-ajax.php", get(mock_sse_toolcall));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

async fn mock_sse_toolcall(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    if q.get("cache_key").map(|s| s.as_str()) != Some("aipkit_sse_testkey") {
        return Sse::new(futures::stream::iter(vec![Ok::<_, Infallible>(
            Event::default()
                .event("error")
                .data(r#"{"error":"no cache"}"#),
        )]));
    }
    // 分片发出，含被切断的 fence（验证 hold-back）
    let events = vec![
        Ok::<_, Infallible>(
            Event::default()
                .event("message_start")
                .data(r#"{"message_id":"m"}"#),
        ),
        Ok(Event::default().data(r#"{"delta":"让我查一下。"}"#)),
        Ok(Event::default().data(r#"{"delta":"\n``"}"#)),
        Ok(Event::default().data(r#"{"delta":"`tool\n"}"#)),
        Ok(Event::default()
            .data(r#"{"delta":"{\"name\":\"get_weather\",\"arguments\":{\"city\":\"北京\"}}\n"}"#)),
        Ok(Event::default().data(r#"{"delta":"```"}"#)),
        Ok(Event::default().data(r#"{"delta":"\n查完了。"}"#)),
        Ok(Event::default().event("done").data(r#"{"finished":true}"#)),
    ];
    Sse::new(futures::stream::iter(events))
}

/// OpenAI 非流式：客户端传 tools → 模型输出 tool 块 → 响应含 tool_calls。
#[tokio::test]
async fn openai_tool_call_nonstream() {
    let base = spawn_gateway_with(spawn_mock_upstream_toolcall().await).await;
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es",
            "messages":[{"role":"user","content":"北京天气？"}],
            "tools":[{"type":"function","function":{"name":"get_weather","description":"查天气","parameters":{"type":"object","properties":{"city":{"type":"string"}}}}}]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let msg = &v["choices"][0]["message"];
    assert_eq!(v["choices"][0]["finish_reason"], "tool_calls", "{v}");
    let tcs = msg["tool_calls"].as_array().expect("应有 tool_calls");
    assert_eq!(tcs.len(), 1, "{v}");
    assert_eq!(tcs[0]["type"], "function");
    assert_eq!(tcs[0]["function"]["name"], "get_weather");
    let args: serde_json::Value =
        serde_json::from_str(tcs[0]["function"]["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(args["city"], "北京");
    // 工具块不应出现在 content
    assert!(
        !msg["content"].as_str().unwrap_or("").contains("```"),
        "工具块泄漏到 content: {v}"
    );
}

/// OpenAI 流式：tool_calls 以 delta 形式产出，且工具块不泄漏。
#[tokio::test]
async fn openai_tool_call_stream() {
    let base = spawn_gateway_with(spawn_mock_upstream_toolcall().await).await;
    let body = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es",
            "messages":[{"role":"user","content":"北京天气？"}],
            "stream":true,
            "tools":[{"type":"function","function":{"name":"get_weather","parameters":{"type":"object"}}}]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        body.contains("\"tool_calls\""),
        "流式应含 tool_calls: {body}"
    );
    assert!(body.contains("get_weather"), "{body}");
    assert!(body.contains("\"finish_reason\":\"tool_calls\""), "{body}");
    // hold-back：原始工具块不得泄漏
    assert!(!body.contains("```tool"), "工具块泄漏到流: {body}");
    assert!(body.contains("让我查一下"), "文本应放行: {body}");
}

/// Anthropic 非流式：tools → tool_use 块 + stop_reason=tool_use。
#[tokio::test]
async fn anthropic_tool_use_nonstream() {
    let base = spawn_gateway_with(spawn_mock_upstream_toolcall().await).await;
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model":"deepseek-es","max_tokens":100,
            "messages":[{"role":"user","content":"北京天气？"}],
            "tools":[{"name":"get_weather","description":"查天气","input_schema":{"type":"object"}}]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["stop_reason"], "tool_use", "{v}");
    let blocks = v["content"].as_array().expect("content 数组");
    let tool = blocks
        .iter()
        .find(|b| b["type"] == "tool_use")
        .expect("应有 tool_use 块");
    assert_eq!(tool["name"], "get_weather");
    assert_eq!(tool["input"]["city"], "北京");
    assert!(tool["id"].as_str().unwrap().starts_with("call_"), "{v}");
}

/// Anthropic 流式：tool_use content_block + input_json_delta。
#[tokio::test]
async fn anthropic_tool_use_stream() {
    let base = spawn_gateway_with(spawn_mock_upstream_toolcall().await).await;
    let body = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model":"deepseek-es","max_tokens":100,
            "messages":[{"role":"user","content":"北京天气？"}],
            "stream":true,
            "tools":[{"name":"get_weather","input_schema":{"type":"object"}}]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("tool_use"), "应含 tool_use 块: {body}");
    assert!(
        body.contains("input_json_delta"),
        "应含 input_json_delta: {body}"
    );
    assert!(body.contains("\"stop_reason\":\"tool_use\""), "{body}");
    assert!(!body.contains("```tool"), "工具块泄漏: {body}");
}

/// 不传 tools 时行为完全不变（回归）。
#[tokio::test]
async fn no_tools_unchanged_behavior() {
    let base = spawn_gateway_with(spawn_mock_upstream_toolcall().await).await;
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model":"deepseek-es",
            "messages":[{"role":"user","content":"hi"}]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // 无 tools：恒 "stop"，无 tool_calls 字段
    assert_eq!(v["choices"][0]["finish_reason"], "stop", "{v}");
    assert!(v["choices"][0]["message"]["tool_calls"].is_null(), "{v}");
}

// ── v0.8.0：余额查询 + 会话管理 ─────────────────────────────

/// mock 上游：支持余额 REST + 会话管理 action。
async fn spawn_mock_upstream_meta() -> String {
    #[derive(serde::Deserialize)]
    struct F {
        #[serde(default)]
        action: String,
        #[serde(default)]
        #[allow(dead_code)]
        session_id: String,
        #[serde(default)]
        #[allow(dead_code)]
        conversation_uuid: String,
    }
    let app = Router::new()
        .route("/", get(mock_home))
        .route(
            "/wp-json/dsgt/v1/balance",
            get(|| async {
                Json(serde_json::json!({"balance": 42, "free": {"remaining": 7}}))
            }),
        )
        .route(
            "/wp-admin/admin-ajax.php",
            post(move |axum::extract::Form(f): axum::extract::Form<F>| async move {
                match f.action.as_str() {
                    "aipkit_get_frontend_chat_nonce" => (
                        axum::http::StatusCode::OK,
                        axum::http::HeaderMap::new(),
                        r#"{"success":true,"data":{"nonce":"n"}}"#.to_string(),
                    ),
                    "aipkit_get_conversations_list" => (
                        axum::http::StatusCode::OK,
                        axum::http::HeaderMap::new(),
                        r#"{"success":true,"data":{"conversations":[{"conversation_uuid":"cv-1","title":"第一条"},{"uuid":"cv-2","name":"第二条"}]}}"#.to_string(),
                    ),
                    "aipkit_delete_single_conversation" => (
                        axum::http::StatusCode::OK,
                        axum::http::HeaderMap::new(),
                        r#"{"success":true}"#.to_string(),
                    ),
                    _ => (
                        axum::http::StatusCode::OK,
                        axum::http::HeaderMap::new(),
                        r#"{"success":false}"#.to_string(),
                    ),
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{}", addr)
}

#[tokio::test]
async fn balance_endpoint_returns_upstream_quota() {
    let base = spawn_gateway_with(spawn_mock_upstream_meta().await).await;
    let v: serde_json::Value = reqwest::get(format!("{base}/v1/balance"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["balance"], 42, "{v}");
    assert_eq!(v["free"]["remaining"], 7, "{v}");
}

#[tokio::test]
async fn conversations_list_normalized() {
    let base = spawn_gateway_with(spawn_mock_upstream_meta().await).await;
    let v: serde_json::Value = reqwest::get(format!("{base}/v1/conversations"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["object"], "list", "{v}");
    let arr = v["data"].as_array().unwrap();
    assert_eq!(arr.len(), 2, "{v}");
    // 归一化：conversation_uuid || uuid → id
    assert_eq!(arr[0]["id"], "cv-1", "{v}");
    assert_eq!(arr[1]["id"], "cv-2", "{v}");
    assert_eq!(arr[0]["title"], "第一条", "{v}");
}

#[tokio::test]
async fn conversation_delete_requires_id() {
    let base = spawn_gateway_with(spawn_mock_upstream_meta().await).await;
    // 缺 id → 400
    let r = reqwest::Client::new()
        .delete(format!("{base}/v1/conversations"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "缺 id 应 400");

    // 带 id → 成功
    let r2 = reqwest::Client::new()
        .delete(format!("{base}/v1/conversations?id=cv-1"))
        .send()
        .await
        .unwrap();
    assert_eq!(r2.status(), 200, "{}", r2.status());
    let v: serde_json::Value = r2.json().await.unwrap();
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["deleted"], "cv-1", "{v}");
}

// ── 契约防坑：请求体反序列化失败必须返回协议原生错误结构 ──────────

/// Anthropic 端点收到畸形请求体 → 必须是 Anthropic 错误结构（非 axum 422 纯文本）。
#[tokio::test]
async fn anthropic_malformed_body_is_anthropic_shaped() {
    let base = spawn_gateway().await;
    // 缺 messages 字段
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .header("content-type", "application/json")
        .body(r#"{"model":"deepseek-es","max_tokens":10}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "应为 400，实际 {}", r.status());
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["type"], "error", "缺顶层 type:error: {v}");
    assert!(v["error"]["message"].is_string(), "{v}");
}

/// Anthropic 端点完全非法 JSON → 同为 Anthropic 结构。
#[tokio::test]
async fn anthropic_invalid_json_is_anthropic_shaped() {
    let base = spawn_gateway().await;
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .header("content-type", "application/json")
        .body("not json at all")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "实际 {}", r.status());
    let v: serde_json::Value = r.json().await.unwrap();
    assert_eq!(v["type"], "error", "{v}");
}

/// OpenAI 端点畸形请求体 → 必须是 OpenAI 错误结构（非 422 纯文本）。
#[tokio::test]
async fn openai_malformed_body_is_openai_shaped() {
    let base = spawn_gateway().await;
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .header("content-type", "application/json")
        .body(r#"{"model":"deepseek-es"}"#) // 缺 messages
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400, "应为 400，实际 {}", r.status());
    let v: serde_json::Value = r.json().await.unwrap();
    assert!(v["error"].is_object(), "应为 OpenAI 错误体: {v}");
    assert!(v.get("type").is_none(), "OpenAI 不应有顶层 type: {v}");
}

/// Claude Code 常发的额外字段必须被容忍（不因未知字段拒绝）。
#[tokio::test]
async fn extra_fields_are_tolerated() {
    let base = spawn_gateway().await;
    // Anthropic：metadata / anthropic_version / tools:[] / system 数组
    let r = reqwest::Client::new()
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model":"deepseek-es","max_tokens":50,
            "system":[{"type":"text","text":"sys"}],
            "messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}],
            "tools":[],
            "metadata":{"user_id":"x"},
            "anthropic_version":"2023-06-01"
        }))
        .send()
        .await
        .unwrap();
    // 应通过解析（可能因上游失败返回 502，但绝不应是 400/422 参数错误）
    assert_ne!(r.status(), 400, "额外字段不应导致 400");
    assert_ne!(r.status(), 422, "额外字段不应导致 422");
}
