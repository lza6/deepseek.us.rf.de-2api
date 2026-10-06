//! HTTP 路由与处理器：OpenAI + Anthropic 兼容端点 + 管理端点。

use crate::auth::check_auth;
use crate::cache::ResponseCache;
use crate::config::Config;
use crate::errors::{AppError, AppResult};
use crate::features;
use crate::ledger::{self, Ledger, UsageRecord};
use crate::models::{self, DEFAULT_MODEL};
use crate::protocol::anthropic::{self as anth};
use crate::protocol::openai::{self as oai};
use crate::replay::ReplayStore;
use crate::session::SessionStore;
use crate::upstream::UpstreamClient;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct AppState {
    pub cfg: Config,
    pub upstream: Arc<UpstreamClient>,
    pub sessions: SessionStore,
    /// 请求级响应缓存
    pub cache: ResponseCache,
    /// 用量账本（SQLite）
    pub ledger: Ledger,
    /// 断线重放缓冲
    pub replay: ReplayStore,
}

pub type SharedState = Arc<AppState>;

pub fn build_router(state: SharedState) -> Router {
    // 业务端点：限流/并发仅作用于 API，不拖累 /healthz 观测端点
    let mut api = Router::new()
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(openai_chat))
        .route("/v1/messages", post(anthropic_messages))
        .route("/v1/messages/count_tokens", post(count_tokens));

    if state.cfg.rate_limit_per_sec > 0 {
        let rl = RateLimiter::new(state.cfg.rate_limit_per_sec);
        api = api.layer(axum::middleware::from_fn_with_state(rl, rate_limit_mw));
    }
    if state.cfg.max_concurrency > 0 {
        // GlobalConcurrencyLimitLayer 在 clone 后共享同一 Semaphore，
        // 保证跨路由的**全局**并发上限（ConcurrencyLimitLayer 是 per-route，会放大 N 倍）。
        api = api.layer(tower::limit::GlobalConcurrencyLimitLayer::new(
            state.cfg.max_concurrency,
        ));
    }

    let mut router = Router::new()
        .route("/healthz", get(healthz))
        // 控制台（自校验 admin_enabled/admin_token）
        .route("/admin", get(crate::admin::admin_page))
        .route("/admin/api/status", get(crate::admin::admin_status))
        .merge(api)
        .with_state(state.clone());

    if !state.cfg.cors_allow_origins.is_empty() {
        use tower_http::cors::{Any, CorsLayer};
        let origins: Vec<axum::http::HeaderValue> = state
            .cfg
            .cors_allow_origins
            .iter()
            .filter_map(|o| o.parse().ok())
            .collect();
        let cors = CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(Any)
            .allow_headers(Any);
        router = router.layer(cors);
    }
    router
}

/// 轻量固定窗口限流器（按秒计数）。
#[derive(Clone)]
struct RateLimiter {
    per_sec: u64,
    inner: Arc<std::sync::Mutex<(Instant, u64)>>,
}

impl RateLimiter {
    fn new(per_sec: u64) -> Self {
        RateLimiter {
            per_sec,
            inner: Arc::new(std::sync::Mutex::new((Instant::now(), 0))),
        }
    }

    /// 是否放行（固定窗口：每满 1 秒重置计数）。
    fn allow(&self) -> bool {
        let mut g = self.inner.lock().unwrap();
        let now = Instant::now();
        if now.duration_since(g.0) >= Duration::from_secs(1) {
            g.0 = now;
            g.1 = 0;
        }
        if g.1 >= self.per_sec {
            return false;
        }
        g.1 += 1;
        true
    }
}

async fn rate_limit_mw(State(rl): State<RateLimiter>, req: Request, next: Next) -> Response {
    if !rl.allow() {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": {
                    "message": "本机限流：请求过于频繁，请稍后重试",
                    "type": "rate_limit_error",
                    "code": "rate_limit_error",
                }
            })),
        )
            .into_response();
    }
    next.run(req).await
}

// ── 健康检查 ────────────────────────────────────────────

async fn healthz(State(state): State<SharedState>) -> Response {
    Json(serde_json::json!({
        "status": "ok",
        "upstream": state.cfg.upstream_base_url,
        "bot_id": state.cfg.bot_id,
        "cf_solver": state.cfg.cf_solver_url,
        "version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}

// ── 模型列表 ────────────────────────────────────────────

async fn list_models(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> AppResult<Json<serde_json::Value>> {
    check_auth(&state.cfg, &headers)?;
    let data: Vec<serde_json::Value> = models::catalog()
        .into_iter()
        .map(|m| {
            serde_json::json!({
                "id": m.id,
                "object": "model",
                "created": 1700000000,
                "owned_by": "deepseek.es",
                "label": m.label,
                "family": m.family,
                "context_window": m.context_window,
                "provider": m.provider,
                "default": m.default,
                "routable": m.routable,
                "alias_of": m.alias_of,
            })
        })
        .collect();
    Ok(Json(serde_json::json!({
        "object": "list",
        "data": data,
    })))
}

// ── OpenAI /v1/chat/completions ─────────────────────────

async fn openai_chat(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<oai::ChatRequest>,
) -> AppResult<Response> {
    check_auth(&state.cfg, &headers)?;
    let t0 = Instant::now();
    let key_id = ledger::key_id(&headers);
    let model_id = req
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let meta = models::resolve_model(&model_id, &state.cfg.default_model);
    let raw_prompt = oai::messages_to_prompt(&req)?;
    // P3-6：语言/风格指令注入
    let prompt = features::inject_system_prompt(&raw_prompt, &state.cfg.system_prompt_suffix);
    let stream = req.stream.unwrap_or(false);
    // P3-7：伪工具说明注入（仅当启用且非流式时提示模型可调用）
    let cache_key = ResponseCache::key(&model_id, &prompt);

    let session_key = req.user.clone().or_else(|| {
        headers
            .get("x-session-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    // 缓存仅在"无会话历史"（纯单轮）时启用，避免与上游多轮上下文语义冲突
    let cacheable = session_key.is_none();
    let (_sid, conv_uuid) = state.sessions.get_or_create(session_key.as_deref());

    // P3-2：缓存命中（仅非流式 + 可缓存）
    if !stream && cacheable {
        if let Some(hit) = state.cache.get(cache_key) {
            state
                .ledger
                .record(UsageRecord {
                    ts: now_secs(),
                    model: model_id.clone(),
                    key_id,
                    prompt_tokens: estimate_tokens(&prompt),
                    completion_tokens: estimate_tokens(&hit),
                    latency_ms: t0.elapsed().as_millis() as u64,
                    status: 200,
                    stream: false,
                    cached: true,
                })
                .await;
            return Ok(Json(chat_completion(&model_id, hit, &prompt)).into_response());
        }
    }

    // 确保认证
    state.upstream.ensure_authed().await?;

    if stream {
        let (events, id) = start_stream(&state, &prompt, &conv_uuid, &model_id, &meta).await?;
        let model = model_id.clone();
        let id_head = id.clone();
        let model_head = model.clone();
        // P3-5：断线重放缓冲
        let replay = state.replay.clone();
        let rid = id.clone();
        let head = futures::stream::once({
            let replay = replay.clone();
            let rid = rid.clone();
            async move {
                let fc = oai::first_chunk(&id_head, &model_head);
                let data = serde_json::to_string(&fc).unwrap();
                let seq = replay.push(&rid, format!("data: {data}"));
                Ok::<_, Infallible>(Event::default().id(seq.to_string()).data(data))
            }
        });
        let replay_body = replay.clone();
        let rid_body = rid.clone();
        let out = head
            .chain(events.map(move |item| -> Result<Event, Infallible> {
                let ev = match item {
                    Ok(t) => t,
                    Err(e) => oai::Translated::Error(e.to_string()),
                };
                let (data, is_done) = match ev {
                    oai::Translated::Delta(text) => (
                        serde_json::to_string(&oai::content_chunk(&id, &model, &text)).unwrap(),
                        false,
                    ),
                    oai::Translated::Done => (
                        serde_json::to_string(&oai::stop_chunk(&id, &model)).unwrap(),
                        true,
                    ),
                    oai::Translated::Error(e) => (
                        serde_json::json!({"error":{"message":e,"type":"upstream_error"}})
                            .to_string(),
                        false,
                    ),
                    oai::Translated::Quota(m) => (
                        serde_json::json!({"error":{"message":m,"type":"rate_limit_error"}})
                            .to_string(),
                        false,
                    ),
                };
                let seq = replay_body.push(&rid_body, format!("data: {data}"));
                if is_done {
                    replay_body.finish(&rid_body);
                }
                Ok(Event::default().id(seq.to_string()).data(data))
            }))
            .chain(futures::stream::once({
                let replay = replay.clone();
                let rid = rid.clone();
                async move {
                    replay.finish(&rid);
                    Ok(Event::default().data("[DONE]"))
                }
            }));
        return Ok(Sse::new(out).into_response());
    }

    // 非流式：聚合
    let (mut events, id) = start_stream(&state, &prompt, &conv_uuid, &model_id, &meta).await?;
    let mut full = String::new();
    while let Some(item) = events.next().await {
        match item {
            Ok(oai::Translated::Delta(t)) => {
                // P2：非流式输出上限保护
                if full.len() + t.len() > state.cfg.max_response_bytes {
                    return Err(AppError::Upstream("响应体超过上限".into()));
                }
                full.push_str(&t);
            }
            Ok(oai::Translated::Done) => break,
            Ok(oai::Translated::Error(e)) => {
                if e == "__TS_REQUIRED__" {
                    return Err(AppError::TsRequired);
                }
                return Err(AppError::UpstreamStream(e));
            }
            Ok(oai::Translated::Quota(m)) => return Err(AppError::QuotaExhausted(m)),
            Err(e) => return Err(e),
        }
    }
    // P3-7：解析伪工具调用 → 本地执行 → 追加结果文本
    let parsed = features::parse_tool_calls(&full);
    let final_text = if parsed.calls.is_empty() {
        full.clone()
    } else {
        let mut t = parsed.text.clone();
        t.push_str("\n\n");
        for call in &parsed.calls {
            let r = features::execute_tool(call);
            t.push_str(&format!("[tool:{}] {}\n", call.name, r));
        }
        t.trim().to_string()
    };

    // P3-2：写入缓存
    if cacheable {
        state.cache.put(cache_key, final_text.clone());
    }

    let pt = estimate_tokens(&prompt);
    let ct = estimate_tokens(&final_text);
    // P3-3：账本
    state
        .ledger
        .record(UsageRecord {
            ts: now_secs(),
            model: model_id.clone(),
            key_id,
            prompt_tokens: pt,
            completion_tokens: ct,
            latency_ms: t0.elapsed().as_millis() as u64,
            status: 200,
            stream: false,
            cached: false,
        })
        .await;
    Ok(Json(oai::ChatCompletion {
        id,
        object: "chat.completion".into(),
        created: now_secs(),
        model: model_id,
        choices: vec![oai::CompletionChoice {
            index: 0,
            message: oai::AssistantMessage {
                role: "assistant".into(),
                content: final_text,
            },
            finish_reason: "stop".into(),
        }],
        usage: oai::Usage {
            prompt_tokens: pt,
            completion_tokens: ct,
            total_tokens: pt + ct,
        },
    })
    .into_response())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn chat_completion(model: &str, content: String, prompt: &str) -> oai::ChatCompletion {
    let pt = estimate_tokens(prompt);
    let ct = estimate_tokens(&content);
    oai::ChatCompletion {
        id: format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
        object: "chat.completion".into(),
        created: now_secs(),
        model: model.to_string(),
        choices: vec![oai::CompletionChoice {
            index: 0,
            message: oai::AssistantMessage {
                role: "assistant".into(),
                content,
            },
            finish_reason: "stop".into(),
        }],
        usage: oai::Usage {
            prompt_tokens: pt,
            completion_tokens: ct,
            total_tokens: pt + ct,
        },
    }
}

/// 翻译后的事件流类型别名。
type TranslatedStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = AppResult<oai::Translated>> + Send>>;

/// 启动一次上游流（cache_message → stream_chat），返回翻译后的事件流与响应 id。
///
/// `ts_required` 自愈：上游可能返回"安全校验失效"信号（首个事件即为
/// `Translated::Error("__TS_REQUIRED__")`）。此时尚未向下游输出任何内容，
/// 故可安全地强制重认证后**重试一次**。中途（已输出后）再遇该信号则不重试，
/// 交由下游错误帧处理，避免重复输出。
async fn start_stream(
    state: &SharedState,
    prompt: &str,
    conv_uuid: &str,
    _model_id: &str,
    _meta: &models::ModelMeta,
) -> AppResult<(TranslatedStream, String)> {
    let (mut stream, id) = start_stream_once(state, prompt, conv_uuid).await?;

    // 探测首个事件，判断是否需要安全校验重试。
    let first = stream.next().await;
    if let Some(Ok(oai::Translated::Error(ref e))) = first {
        if e == "__TS_REQUIRED__" {
            tracing::warn!("上游要求重新安全校验，强制重认证后重试");
            state.upstream.force_reauth().await?;
            let (stream2, id2) = start_stream_once(state, prompt, conv_uuid).await?;
            return Ok((stream2, id2));
        }
    }
    // 无需重试：把已探测的事件拼回流首，保持原顺序。
    let head = futures::stream::iter(first);
    Ok((Box::pin(head.chain(stream)), id))
}

/// 单次上游流启动（不含重试）。cache_key 一次性，每次调用都会重新申请。
async fn start_stream_once(
    state: &SharedState,
    prompt: &str,
    conv_uuid: &str,
) -> AppResult<(TranslatedStream, String)> {
    let cache_key = state.upstream.cache_message(prompt).await?;
    let sid = conv_uuid.to_string();
    let raw = state
        .upstream
        .stream_chat(&cache_key, &sid, conv_uuid)
        .await?;
    let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
    let mapped = raw.filter_map(|ev| async move {
        match ev {
            Ok(e) => oai::translate_event(&e).map(Ok),
            Err(e) => Some(Err(e)),
        }
    });
    Ok((Box::pin(mapped), id))
}

// ── Anthropic /v1/messages ──────────────────────────────

async fn anthropic_messages(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<anth::MessagesRequest>,
) -> AppResult<Response> {
    check_auth(&state.cfg, &headers)?;
    let model_id = req
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let meta = models::resolve_model(&model_id, &state.cfg.default_model);
    let prompt = anth::messages_to_prompt(&req).map_err(AppError::BadRequest)?;
    let stream = req.stream.unwrap_or(false);

    let session_key = headers
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let (_sid, conv_uuid) = state.sessions.get_or_create(session_key.as_deref());

    state.upstream.ensure_authed().await?;
    let (events, id) = start_stream(&state, &prompt, &conv_uuid, &model_id, &meta).await?;

    if stream {
        let model = model_id.clone();
        let msg_id = id.clone();
        let mut state_started = false;
        let mut block_started = false;
        let out = events.flat_map(move |item| {
            let model = model.clone();
            let msg_id = msg_id.clone();
            let mut evs: Vec<Result<Event, Infallible>> = Vec::new();
            // 首帧 message_start + content_block_start
            if !state_started {
                state_started = true;
                let ms = anth::MessageStartEvent {
                    kind: anth::SSE_EVENT_MESSAGE_START,
                    message: anth::MessageBody {
                        id: msg_id.clone(),
                        kind: "message",
                        role: "assistant",
                        model: model.clone(),
                        content: vec![],
                        stop_reason: None,
                        stop_sequence: None,
                        usage: anth::AnthropicUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                        },
                    },
                };
                evs.push(Ok(sse_named(anth::SSE_EVENT_MESSAGE_START, &ms)));
            }
            match item {
                Ok(oai::Translated::Delta(text)) => {
                    if !block_started {
                        block_started = true;
                        let cbs = anth::ContentBlockStart {
                            kind: anth::SSE_EVENT_CONTENT_BLOCK_START,
                            index: 0,
                            content_block: anth::ContentBlock {
                                kind: "text",
                                text: String::new(),
                            },
                        };
                        evs.push(Ok(sse_named(anth::SSE_EVENT_CONTENT_BLOCK_START, &cbs)));
                    }
                    let cbd = anth::ContentBlockDelta {
                        kind: anth::SSE_EVENT_CONTENT_BLOCK_DELTA,
                        index: 0,
                        delta: anth::TextDelta {
                            kind: "text_delta",
                            text,
                        },
                    };
                    evs.push(Ok(sse_named(anth::SSE_EVENT_CONTENT_BLOCK_DELTA, &cbd)));
                }
                Ok(oai::Translated::Done) => {
                    if block_started {
                        let cbs = anth::ContentBlockStop {
                            kind: anth::SSE_EVENT_CONTENT_BLOCK_STOP,
                            index: 0,
                        };
                        evs.push(Ok(sse_named(anth::SSE_EVENT_CONTENT_BLOCK_STOP, &cbs)));
                    }
                    let md = anth::MessageDelta {
                        kind: anth::SSE_EVENT_MESSAGE_DELTA,
                        delta: anth::DeltaStop {
                            stop_reason: "end_turn".into(),
                            stop_sequence: None,
                        },
                        usage: anth::AnthropicUsage {
                            input_tokens: 0,
                            output_tokens: 0,
                        },
                    };
                    evs.push(Ok(sse_named(anth::SSE_EVENT_MESSAGE_DELTA, &md)));
                    let ms = anth::MessageStop {
                        kind: anth::SSE_EVENT_MESSAGE_STOP,
                    };
                    evs.push(Ok(sse_named(anth::SSE_EVENT_MESSAGE_STOP, &ms)));
                }
                Ok(oai::Translated::Error(e)) => {
                    let body = serde_json::json!({
                        "type": "error",
                        "error": {"type": "upstream_error", "message": e}
                    });
                    evs.push(Ok(Event::default().event("error").data(body.to_string())));
                }
                Ok(oai::Translated::Quota(m)) => {
                    let body = serde_json::json!({
                        "type": "error",
                        "error": {"type": "rate_limit_error", "message": m}
                    });
                    evs.push(Ok(Event::default().event("error").data(body.to_string())));
                }
                Err(e) => {
                    let body = serde_json::json!({
                        "type": "error",
                        "error": {"type": "upstream_error", "message": e.to_string()}
                    });
                    evs.push(Ok(Event::default().event("error").data(body.to_string())));
                }
            }
            futures::stream::iter(evs)
        });
        return Ok(Sse::new(out).into_response());
    }

    // 非流式
    let (mut events, id) = start_stream(&state, &prompt, &conv_uuid, &model_id, &meta).await?;
    let mut full = String::new();
    while let Some(item) = events.next().await {
        match item {
            Ok(oai::Translated::Delta(t)) => full.push_str(&t),
            Ok(oai::Translated::Done) => break,
            Ok(oai::Translated::Error(e)) => {
                return Err(AppError::UpstreamStream(e));
            }
            Ok(oai::Translated::Quota(m)) => return Err(AppError::QuotaExhausted(m)),
            Err(e) => return Err(e),
        }
    }
    let body = serde_json::json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model_id,
        "content": [{"type": "text", "text": full}],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {"input_tokens": estimate_tokens(&prompt), "output_tokens": estimate_tokens(&full)},
    });
    Ok(Json(body).into_response())
}

fn sse_named<T: serde::Serialize>(name: &str, payload: &T) -> Event {
    Event::default()
        .event(name)
        .data(serde_json::to_string(payload).unwrap_or_default())
}

// ── token 估算 ──────────────────────────────────────────

async fn count_tokens(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<anth::MessagesRequest>,
) -> AppResult<Json<serde_json::Value>> {
    check_auth(&state.cfg, &headers)?;
    let text = match anth::messages_to_prompt(&req) {
        Ok(t) => t,
        Err(_) => {
            // 估算时容忍空
            req.messages
                .iter()
                .map(|m| m.text())
                .collect::<Vec<_>>()
                .join("\n")
        }
    };
    let tokens = estimate_tokens(&text);
    Ok(Json(serde_json::json!({ "input_tokens": tokens })))
}

/// 粗略 token 估算：CJK 字符约 1 token/字，其它约 1 token/4 字符（P2-8：修正中文严重低估）。
fn estimate_tokens(text: &str) -> u32 {
    let mut cjk = 0u64;
    let mut other = 0u64;
    for ch in text.chars() {
        if is_cjk(ch) {
            cjk += 1;
        } else {
            other += 1;
        }
    }
    (cjk + other.div_ceil(4)).max(1) as u32
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF        // 日文假名
        | 0x3400..=0x4DBF      // CJK 扩展 A
        | 0x4E00..=0x9FFF      // CJK 基本汉字
        | 0xF900..=0xFAFF      // CJK 兼容
        | 0xAC00..=0xD7AF      // 韩文音节
        | 0x20000..=0x2FA1F    // CJK 扩展 B+
    )
}

/// 会话 TTL 常量（供 main 使用）。
pub const SESSION_TTL_SECS: u64 = 3600;
