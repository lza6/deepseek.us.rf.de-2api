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
fn make_state(cfg: deepseek_es_2api::Config) -> Arc<deepseek_es_2api::api::AppState> {
    let client = Arc::new(deepseek_es_2api::UpstreamClient::new(cfg.clone()).unwrap());
    Arc::new(deepseek_es_2api::api::AppState {
        cfg: cfg.clone(),
        upstream: client,
        sessions: deepseek_es_2api::session::SessionStore::new(Duration::from_secs(60)),
        cache: deepseek_es_2api::cache::ResponseCache::new(
            cfg.cache_ttl_secs,
            cfg.cache_max_entries,
            cfg.cache_min_chars,
        ),
        ledger: deepseek_es_2api::ledger::Ledger::open(&cfg.ledger_path).unwrap(),
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
