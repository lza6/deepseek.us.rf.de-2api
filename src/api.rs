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
use axum::extract::{Query, Request, State};
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
        .route("/v1/messages/count_tokens", post(count_tokens))
        // P3-5：断线重放（配合流式响应的 x-response-id）
        .route("/v1/responses/{id}", get(replay_response))
        // v0.8.0：上游配额/余额（网关身份）
        .route("/v1/balance", get(balance))
        // v0.8.0：会话管理（列出/删除上游会话）
        .route(
            "/v1/conversations",
            get(list_conversations).delete(delete_conversation),
        );

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

/// OpenAI 端点入口：手动解析请求体，保证**反序列化失败**也返回 OpenAI 错误结构
/// （而非 axum `Json` 提取器的 422 + 纯文本——SDK 无法解析）。
async fn openai_chat(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let req: oai::ChatRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return AppError::BadRequest(format!("请求体解析失败: {e}")).into_response();
        }
    };
    openai_chat_inner(state, headers, req)
        .await
        .unwrap_or_else(|e| e.into_response())
}

async fn openai_chat_inner(
    state: SharedState,
    headers: HeaderMap,
    req: oai::ChatRequest,
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
    // v2.0.0：协议级工具——客户端声明了 tools 则注入工具说明（由客户端执行工具）
    let tool_defs = oai::tool_defs(&req);
    let prompt = if let Some(tp) = crate::tools::render_tool_prompt(&tool_defs) {
        // 协议级工具模式：工具说明 + （可选）语言指令
        let base =
            features::inject_prompt_prefixes(&raw_prompt, &state.cfg.system_prompt_suffix, false);
        format!("{tp}\n\n{base}")
    } else {
        // P3-6 + H4：语言/风格指令 + 伪工具说明注入
        features::inject_prompt_prefixes(
            &raw_prompt,
            &state.cfg.system_prompt_suffix,
            state.cfg.pseudo_tools_enabled,
        )
    };
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
        // P3-3：流式请求也需要入账（此前仅非流式被记录）
        let ledger_s = state.ledger.clone();
        let model_s = model.clone();
        let key_s = key_id.clone();
        let prompt_s = prompt.clone();
        let t0_s = t0;
        // 用 Arc<AtomicUsize> 在 map 闭包与末尾 once 之间共享完成字符数
        let acc = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let acc_c = acc.clone();
        // M7：跟踪是否已收到 Done（据此决定异常结束时是否补 finish_reason 帧）
        let oai_finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fin_o = oai_finished.clone();
        // M7 收尾链需要 id/model（map 闭包会 move 走，故先克隆）
        let id_m7 = id.clone();
        let model_m7 = model.clone();
        // v2.0.0：工具模式下的流式 hold-back 过滤器
        let tool_mode = !tool_defs.is_empty();
        let mut filter = crate::tools::StreamToolFilter::new();
        let out = head
            .chain(
                events
                    .map(move |item| -> Vec<Result<Event, Infallible>> {
                let ev = match item {
                    Ok(t) => t,
                    Err(e) => oai::Translated::Error(e.to_string()),
                };
                let mut evs: Vec<Result<Event, Infallible>> = Vec::new();
                let mut push_data = |data: String| {
                    let seq = replay_body.push(&rid_body, format!("data: {data}"));
                    evs.push(Ok(Event::default().id(seq.to_string()).data(data)));
                };
                match ev {
                    oai::Translated::Delta(text) => {
                        acc_c.fetch_add(text.chars().count(), std::sync::atomic::Ordering::Relaxed);
                        if tool_mode {
                            // hold-back：只放行工具块之外的文本
                            if let crate::tools::FilterOut::Text(t) = filter.push(&text) {
                                if !t.is_empty() {
                                    let data =
                                        serde_json::to_string(&oai::content_chunk(&id, &model, &t))
                                            .unwrap();
                                    push_data(data);
                                }
                            }
                        } else {
                            let data =
                                serde_json::to_string(&oai::content_chunk(&id, &model, &text)).unwrap();
                            push_data(data);
                        }
                    }
                    oai::Translated::Done => {
                        fin_o.store(true, std::sync::atomic::Ordering::Relaxed);
                        if tool_mode {
                            // 取出暂扣文本 + 解析出的工具调用
                            let (tail, calls) = filter.push_finish();
                            if !tail.is_empty() {
                                let data = serde_json::to_string(&oai::content_chunk(
                                    &id, &model, &tail,
                                ))
                                .unwrap();
                                push_data(data);
                            }
                            for (i, inv) in calls.iter().enumerate() {
                                let data = serde_json::to_string(&oai::tool_call_chunk(
                                    &id,
                                    &model,
                                    i as u32,
                                    &inv.id,
                                    &inv.name,
                                    &inv.arguments.to_string(),
                                ))
                                .unwrap();
                                push_data(data);
                            }
                            let reason = if calls.is_empty() { "stop" } else { "tool_calls" };
                            let data =
                                serde_json::to_string(&oai::stop_chunk_reason(&id, &model, reason))
                                    .unwrap();
                            push_data(data);
                            replay_body.finish(&rid_body);
                        } else {
                            let data = serde_json::to_string(&oai::stop_chunk(&id, &model)).unwrap();
                            let seq = replay_body.push(&rid_body, format!("data: {data}"));
                            evs.push(Ok(Event::default().id(seq.to_string()).data(data)));
                            replay_body.finish(&rid_body);
                        }
                    }
                    oai::Translated::Error(e) => {
                        let data =
                            serde_json::json!({"error":{"message":e,"type":"upstream_error"}})
                                .to_string();
                        push_data(data);
                    }
                    oai::Translated::Quota(m) => {
                        let data =
                            serde_json::json!({"error":{"message":m,"type":"rate_limit_error"}})
                                .to_string();
                        push_data(data);
                    }
                    // M3：中途要求安全校验——发结构化错误（不泄漏哨兵字符串）
                    oai::Translated::TsRequired => {
                        let data = serde_json::json!({"error":{"message":"上游要求重新安全校验，请重试","type":"api_error"}}).to_string();
                        push_data(data);
                    }
                }
                evs
                    })
                    .map(futures::stream::iter)
                    .flatten(),
            )
            // M7：上游异常结束（未收到 Done）时补一帧带 finish_reason 的结束块
            .chain(
                futures::stream::once({
                    let oai_finished = oai_finished.clone();
                    let id = id_m7.clone();
                    let model = model_m7.clone();
                    async move {
                        if oai_finished.load(std::sync::atomic::Ordering::Relaxed) {
                            Vec::new()
                        } else {
                            let data =
                                serde_json::to_string(&oai::stop_chunk(&id, &model)).unwrap();
                            vec![Ok::<_, Infallible>(Event::default().data(data))]
                        }
                    }
                })
                .map(futures::stream::iter)
                .flatten(),
            )
            .chain(futures::stream::once({
                let replay = replay.clone();
                let rid = rid.clone();
                async move {
                    replay.finish(&rid);
                    Ok(Event::default().data("[DONE]"))
                }
            }))
            .chain(futures::stream::once(async move {
                // 流结束记账（异步、失败不阻断）
                let chars = acc.load(std::sync::atomic::Ordering::Relaxed);
                ledger_s
                    .record(UsageRecord {
                        ts: now_secs(),
                        model: model_s,
                        key_id: key_s,
                        prompt_tokens: estimate_tokens(&prompt_s),
                        completion_tokens: (chars / 2).max(1) as u32,
                        latency_ms: t0_s.elapsed().as_millis() as u64,
                        status: 200,
                        stream: true,
                        cached: false,
                    })
                    .await;
                Ok::<_, Infallible>(Event::default().comment(""))
            }));
        // 暴露响应 id，供客户端断线后调用 /v1/responses/<id> 重放
        let mut resp = Sse::new(out).into_response();
        if let Ok(v) = axum::http::HeaderValue::from_str(&rid) {
            resp.headers_mut().insert("x-response-id", v);
        }
        return Ok(resp);
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
                return Err(AppError::UpstreamStream(e));
            }
            // M3：结构化 ts_required（不再用哨兵字符串比较）
            Ok(oai::Translated::TsRequired) => return Err(AppError::TsRequired),
            Ok(oai::Translated::Quota(m)) => return Err(AppError::QuotaExhausted(m)),
            Err(e) => return Err(e),
        }
    }
    // v2.0.0：协议级工具优先——客户端声明 tools 时，产出标准 tool_calls（由客户端执行）
    // 否则回退 P3-7 伪工具（网关本地执行并回填文本）。
    let (final_text, tool_calls, finish_reason) = if !tool_defs.is_empty() {
        let (text, invocations) = crate::tools::parse_invocations(&full);
        if invocations.is_empty() {
            (text, None, "stop".to_string())
        } else {
            let calls: Vec<oai::ToolCallMsg> = invocations
                .iter()
                .map(|inv| oai::ToolCallMsg {
                    id: inv.id.clone(),
                    kind: Some("function".into()),
                    function: oai::ToolCallFunction {
                        name: inv.name.clone(),
                        arguments: inv.arguments.to_string(),
                    },
                })
                .collect();
            (text, Some(calls), "tool_calls".to_string())
        }
    } else {
        let parsed = features::parse_tool_calls(&full);
        let t = if parsed.calls.is_empty() {
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
        (t, None, "stop".to_string())
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
                tool_calls,
            },
            finish_reason,
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
                tool_calls: None,
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

// ── 上游余额（v0.8.0）─────────────────────────────────────

/// `GET /v1/balance`：查询**网关身份**的上游配额/余额。
///
/// 返回上游 `{balance, free:{remaining}}` 原样透传。
/// **注意**：余额绑定网关的浏览器身份（dsts cookie），非下游用户余额。
async fn balance(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    if let Err(e) = check_auth(&state.cfg, &headers) {
        return e.into_response();
    }
    match state.upstream.fetch_balance().await {
        Ok(v) => Json(v).into_response(),
        Err(e) => e.into_response(),
    }
}

// ── 会话管理（v0.8.0）─────────────────────────────────────

/// 从请求头取会话 id（`x-session-id`），缺省用网关默认身份。
fn session_id_of(headers: &HeaderMap) -> String {
    headers
        .get("x-session-id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "gateway".to_string())
}

/// `GET /v1/conversations`：列出上游会话（`?session_id=` 或 `x-session-id` 头）。
async fn list_conversations(State(state): State<SharedState>, headers: HeaderMap) -> Response {
    if let Err(e) = check_auth(&state.cfg, &headers) {
        return e.into_response();
    }
    let sid = session_id_of(&headers);
    match state.upstream.list_conversations(&sid).await {
        Ok(list) => Json(serde_json::json!({"object":"list","data":list})).into_response(),
        Err(e) => e.into_response(),
    }
}

/// `DELETE /v1/conversations?id=<uuid>`：删除单条上游会话。
async fn delete_conversation(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(e) = check_auth(&state.cfg, &headers) {
        return e.into_response();
    }
    let Some(id) = q.get("id").filter(|s| !s.is_empty()) else {
        return AppError::BadRequest("缺少 id 查询参数".into()).into_response();
    };
    let sid = session_id_of(&headers);
    match state.upstream.delete_conversation(&sid, id).await {
        Ok(()) => Json(serde_json::json!({"ok": true,"deleted": id})).into_response(),
        Err(e) => e.into_response(),
    }
}

// ── 断线重放（P3-5）─────────────────────────────────────
/// `GET /v1/responses/{id}`：重放某次流式响应中、`Last-Event-ID` 之后的事件。
///
/// - 响应 id 来自流式响应的 `x-response-id` 头。
/// - `Last-Event-ID` 头（或 `?after=`）指定已收到的最后一个序号。
/// - 返回 `text/event-stream`，逐帧回放缓冲中的 SSE 数据。
/// - 无此响应/已过期 → 409（提示客户端重新发起）。
async fn replay_response(
    State(state): State<SharedState>,
    headers: HeaderMap,
    axum::extract::Path(id): axum::extract::Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(e) = check_auth(&state.cfg, &headers) {
        return e.into_response();
    }
    let after = crate::replay::parse_last_event_id(&headers)
        .or_else(|| q.get("after").and_then(|s| s.parse().ok()))
        .unwrap_or(0);
    match state.replay.replay(&id, after) {
        Some(entries) => {
            let frames: Vec<Result<Event, Infallible>> = entries
                .into_iter()
                .map(|e| {
                    // e.frame 形如 "data: {...}"（可能带多行）；这里作为原始 data 回放
                    let data = e
                        .frame
                        .strip_prefix("data: ")
                        .unwrap_or(&e.frame)
                        .to_string();
                    Ok(Event::default().id(e.seq.to_string()).data(data))
                })
                .collect();
            Sse::new(futures::stream::iter(frames)).into_response()
        }
        None => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": {
                    "message": "响应不存在或重放缓冲已过期，请重新发起请求",
                    "type": "invalid_request_error",
                    "code": "replay_unavailable",
                }
            })),
        )
            .into_response(),
    }
}

/// 翻译后的事件流类型别名。
type TranslatedStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = AppResult<oai::Translated>> + Send>>;

/// 启动一次上游流（cache_message → stream_chat），返回翻译后的事件流与响应 id。
///
/// `ts_required` 自愈：上游可能返回"安全校验失效"信号（首个事件即为
/// `Translated::TsRequired`）。此时尚未向下游输出任何内容，
/// 故可安全地强制重认证后**重试一次**。中途（已输出后）再遇该信号则不重试，
/// 交由下游错误帧处理，避免重复输出。
///
/// **M10**：首事件若为 `Quota`（配额耗尽），此时响应头**尚未发出**，
/// 可提前返回 `AppError::QuotaExhausted`，使流式请求也得到正确的 **HTTP 429**
/// （而非「200 + 错误帧」——后者多数 SDK 不会当作限流处理）。
/// 首事件若为普通 `Error`，同样提前失败（避免「200 + 错误帧」）。
async fn start_stream(
    state: &SharedState,
    prompt: &str,
    conv_uuid: &str,
    _model_id: &str,
    _meta: &models::ModelMeta,
) -> AppResult<(TranslatedStream, String)> {
    let (mut stream, id) = start_stream_once(state, prompt, conv_uuid).await?;

    // 探测首个事件：安全校验重试 / 配额 / 错误。
    let mut first = stream.next().await;
    // 对需要提前失败/重试的首事件做处理（用 take 取出，避免 Clone）。
    match first.take() {
        Some(Ok(oai::Translated::TsRequired)) => {
            tracing::warn!("上游要求重新安全校验，强制重认证后重试");
            state.upstream.force_reauth().await?;
            let (stream2, id2) = start_stream_once(state, prompt, conv_uuid).await?;
            return Ok((stream2, id2));
        }
        // M10：首事件即配额耗尽 → 提前返回 429（响应头尚未发出）
        Some(Ok(oai::Translated::Quota(m))) => {
            tracing::warn!("上游首事件即配额耗尽，返回 429");
            return Err(AppError::QuotaExhausted(m));
        }
        // 首事件即上游错误 → 提前失败（结构化，不泄漏内部细节）
        Some(Ok(oai::Translated::Error(e))) => {
            return Err(AppError::UpstreamStream(e));
        }
        Some(Err(e)) => return Err(e),
        // 其余（Delta/Done）或流为空：放回流首
        other => first = other,
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

/// H3：Anthropic 端点对外错误必须是 Anthropic 结构（顶层 `type:"error"`）。
/// 内部逻辑返回 `AppResult`，此处统一转换为 Anthropic 兼容响应体。
///
/// **契约防坑**：直接接收原始 `Bytes` 而非 `Json<MessagesRequest>`——
/// 否则请求体**反序列化失败**时（如缺 `messages` 字段），axum 的 `Json` 提取器
/// 会返回 **422 + 纯文本**（`Failed to deserialize...`），绕过 Anthropic 错误结构，
/// 导致 Claude Code / Anthropic SDK 解析失败。手动解析可保证所有错误都是 Anthropic 结构。
async fn anthropic_messages(
    State(state): State<SharedState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let req: anth::MessagesRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return AppError::BadRequest(format!("请求体解析失败: {e}")).into_anthropic_response()
        }
    };
    anthropic_messages_inner(state, headers, req)
        .await
        .unwrap_or_else(|e| e.into_anthropic_response())
}

async fn anthropic_messages_inner(
    state: SharedState,
    headers: HeaderMap,
    req: anth::MessagesRequest,
) -> AppResult<Response> {
    check_auth(&state.cfg, &headers)?;
    let model_id = req
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let meta = models::resolve_model(&model_id, &state.cfg.default_model);
    // v2.0.0：协议级工具——客户端声明 tools 则注入工具说明（由客户端执行）
    let tool_defs = anth::tool_defs(&req);
    let raw_prompt = anth::messages_to_prompt(&req).map_err(AppError::BadRequest)?;
    let prompt = if let Some(tp) = crate::tools::render_tool_prompt(&tool_defs) {
        let base =
            features::inject_prompt_prefixes(&raw_prompt, &state.cfg.system_prompt_suffix, false);
        format!("{tp}\n\n{base}")
    } else {
        features::inject_prompt_prefixes(
            &raw_prompt,
            &state.cfg.system_prompt_suffix,
            state.cfg.pseudo_tools_enabled,
        )
    };
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
        // M4：流式 usage 真实化（此前 message_start/message_delta 恒 0/0）
        let prompt_tokens = estimate_tokens(&prompt);
        // H3/M4：用 Arc 在 flat_map 与末尾 once 间共享输出累计与结束标记
        let out_chars = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let out_c = out_chars.clone();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let fin_c = finished.clone();
        // M7：跟踪是否已开过内容块（异常结束时据此决定是否补 content_block_stop）
        let block_open = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let block_open_c = block_open.clone();
        let prompt_tokens_c = prompt_tokens;
        // v2.0.0：工具模式下的流式 hold-back 过滤器（Anthropic 侧）
        let anth_tool_mode = !tool_defs.is_empty();
        let mut anth_filter = crate::tools::StreamToolFilter::new();
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
                            input_tokens: prompt_tokens_c,
                            output_tokens: 0,
                        },
                    },
                };
                evs.push(Ok(sse_named(anth::SSE_EVENT_MESSAGE_START, &ms)));
            }
            match item {
                Ok(oai::Translated::Delta(text)) => {
                    out_c.fetch_add(text.chars().count(), std::sync::atomic::Ordering::Relaxed);
                    // 工具模式：hold-back，只放行工具块之外文本
                    let emit_text = if anth_tool_mode {
                        match anth_filter.push(&text) {
                            crate::tools::FilterOut::Text(t) => t,
                            crate::tools::FilterOut::Hold => String::new(),
                        }
                    } else {
                        text
                    };
                    if !emit_text.is_empty() {
                        if !block_started {
                            block_started = true;
                            block_open_c.store(true, std::sync::atomic::Ordering::Relaxed);
                            let cbs = anth::ContentBlockStart {
                                kind: anth::SSE_EVENT_CONTENT_BLOCK_START,
                                index: 0,
                                content_block: anth::ContentBlock::text(""),
                            };
                            evs.push(Ok(sse_named(anth::SSE_EVENT_CONTENT_BLOCK_START, &cbs)));
                        }
                        let cbd = anth::ContentBlockDelta {
                            kind: anth::SSE_EVENT_CONTENT_BLOCK_DELTA,
                            index: 0,
                            delta: anth::TextDelta {
                                kind: "text_delta",
                                text: emit_text,
                            },
                        };
                        evs.push(Ok(sse_named(anth::SSE_EVENT_CONTENT_BLOCK_DELTA, &cbd)));
                    }
                }
                Ok(oai::Translated::Done) => {
                    fin_c.store(true, std::sync::atomic::Ordering::Relaxed);
                    // 工具模式：冲掉暂扣文本 + 产出 tool_use 块
                    let tool_calls = if anth_tool_mode {
                        let (tail, calls) = anth_filter.push_finish();
                        if !tail.is_empty() {
                            if !block_started {
                                block_started = true;
                                block_open_c.store(true, std::sync::atomic::Ordering::Relaxed);
                                let cbs = anth::ContentBlockStart {
                                    kind: anth::SSE_EVENT_CONTENT_BLOCK_START,
                                    index: 0,
                                    content_block: anth::ContentBlock::text(""),
                                };
                                evs.push(Ok(sse_named(anth::SSE_EVENT_CONTENT_BLOCK_START, &cbs)));
                            }
                            let cbd = anth::ContentBlockDelta {
                                kind: anth::SSE_EVENT_CONTENT_BLOCK_DELTA,
                                index: 0,
                                delta: anth::TextDelta {
                                    kind: "text_delta",
                                    text: tail,
                                },
                            };
                            evs.push(Ok(sse_named(anth::SSE_EVENT_CONTENT_BLOCK_DELTA, &cbd)));
                        }
                        calls
                    } else {
                        Vec::new()
                    };
                    if block_started {
                        let cbs = anth::ContentBlockStop {
                            kind: anth::SSE_EVENT_CONTENT_BLOCK_STOP,
                            index: 0,
                        };
                        evs.push(Ok(sse_named(anth::SSE_EVENT_CONTENT_BLOCK_STOP, &cbs)));
                        block_open_c.store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                    // 每个工具调用产出独立 content_block（tool_use）
                    for (i, inv) in tool_calls.iter().enumerate() {
                        let idx = (i + 1) as u32;
                        let cbs = anth::ContentBlockStart {
                            kind: anth::SSE_EVENT_CONTENT_BLOCK_START,
                            index: idx,
                            content_block: anth::ContentBlock::tool_use(
                                inv.id.clone(),
                                inv.name.clone(),
                                serde_json::json!({}),
                            ),
                        };
                        evs.push(Ok(sse_named(anth::SSE_EVENT_CONTENT_BLOCK_START, &cbs)));
                        // 参数一次性以 input_json_delta 发出（简化，客户端会累积）
                        let ijd = serde_json::json!({
                            "type": "content_block_delta",
                            "index": idx,
                            "delta": {
                                "type": "input_json_delta",
                                "partial_json": inv.arguments.to_string(),
                            }
                        });
                        evs.push(Ok(Event::default()
                            .event(anth::SSE_EVENT_CONTENT_BLOCK_DELTA)
                            .data(ijd.to_string())));
                        let cbe = anth::ContentBlockStop {
                            kind: anth::SSE_EVENT_CONTENT_BLOCK_STOP,
                            index: idx,
                        };
                        evs.push(Ok(sse_named(anth::SSE_EVENT_CONTENT_BLOCK_STOP, &cbe)));
                    }
                    let stop_reason = if tool_calls.is_empty() {
                        "end_turn"
                    } else {
                        "tool_use"
                    };
                    let md = anth::MessageDelta {
                        kind: anth::SSE_EVENT_MESSAGE_DELTA,
                        delta: anth::DeltaStop {
                            stop_reason: stop_reason.into(),
                            stop_sequence: None,
                        },
                        usage: anth::AnthropicUsage {
                            input_tokens: 0,
                            output_tokens: (out_c.load(std::sync::atomic::Ordering::Relaxed) / 2)
                                .max(1) as u32,
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
                        "error": {"type": "api_error", "message": e}
                    });
                    evs.push(Ok(Event::default().event("error").data(body.to_string())));
                }
                // M3：结构化 ts_required（Anthropic 合法类型用 api_error）
                Ok(oai::Translated::TsRequired) => {
                    let body = serde_json::json!({
                        "type": "error",
                        "error": {"type": "api_error", "message": "上游要求重新安全校验，请重试"}
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
                        "error": {"type": "api_error", "message": e.to_string()}
                    });
                    evs.push(Ok(Event::default().event("error").data(body.to_string())));
                }
            }
            futures::stream::iter(evs)
        });
        // M7：上游异常结束（断开/错误）时补发标准结束序列，避免客户端把截断当正常结束。
        let out = out.chain(
            futures::stream::once({
                let finished = finished.clone();
                let block_open = block_open.clone();
                let out_chars = out_chars.clone();
                async move {
                    if finished.load(std::sync::atomic::Ordering::Relaxed) {
                        Vec::new()
                    } else {
                        let mut evs: Vec<Result<Event, Infallible>> = Vec::new();
                        if block_open.load(std::sync::atomic::Ordering::Relaxed) {
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
                                output_tokens: (out_chars
                                    .load(std::sync::atomic::Ordering::Relaxed)
                                    / 2)
                                .max(1) as u32,
                            },
                        };
                        evs.push(Ok(sse_named(anth::SSE_EVENT_MESSAGE_DELTA, &md)));
                        let ms = anth::MessageStop {
                            kind: anth::SSE_EVENT_MESSAGE_STOP,
                        };
                        evs.push(Ok(sse_named(anth::SSE_EVENT_MESSAGE_STOP, &ms)));
                        evs
                    }
                }
            })
            .map(futures::stream::iter)
            .flatten(),
        );
        // P3-3：Anthropic 流式入账（流结束触发；token 用 prompt 估算 + 输出字符估算）
        let ledger_s = state.ledger.clone();
        let model_s = model_id.clone();
        let key_s = ledger::key_id(&headers);
        let prompt_s = prompt.clone();
        let out_chars_l = out_chars.clone();
        let out = out.chain(futures::stream::once(async move {
            let out_tok =
                (out_chars_l.load(std::sync::atomic::Ordering::Relaxed) / 2).max(1) as u32;
            ledger_s
                .record(UsageRecord {
                    ts: now_secs(),
                    model: model_s,
                    key_id: key_s,
                    prompt_tokens: estimate_tokens(&prompt_s),
                    completion_tokens: out_tok,
                    latency_ms: 0,
                    status: 200,
                    stream: true,
                    cached: false,
                })
                .await;
            Ok::<_, Infallible>(Event::default().comment(""))
        }));
        return Ok(Sse::new(out).into_response());
    }

    // 非流式
    let (mut events, id) = start_stream(&state, &prompt, &conv_uuid, &model_id, &meta).await?;
    let mut full = String::new();
    while let Some(item) = events.next().await {
        match item {
            Ok(oai::Translated::Delta(t)) => {
                // M11：与 OpenAI 非流式对齐的响应上限保护
                if full.len() + t.len() > state.cfg.max_response_bytes {
                    return Err(AppError::Upstream("响应体超过上限".into()));
                }
                full.push_str(&t);
            }
            Ok(oai::Translated::Done) => break,
            Ok(oai::Translated::Error(e)) => {
                return Err(AppError::UpstreamStream(e));
            }
            // M3：结构化 ts_required
            Ok(oai::Translated::TsRequired) => return Err(AppError::TsRequired),
            Ok(oai::Translated::Quota(m)) => return Err(AppError::QuotaExhausted(m)),
            Err(e) => return Err(e),
        }
    }
    // v2.0.0：协议级工具优先——声明 tools 时产出标准 tool_use 块
    let (content, stop_reason) = if !tool_defs.is_empty() {
        let (text, invocations) = crate::tools::parse_invocations(&full);
        let mut blocks: Vec<serde_json::Value> = Vec::new();
        if !text.trim().is_empty() {
            blocks.push(serde_json::json!({"type":"text","text":text}));
        }
        for inv in &invocations {
            blocks.push(serde_json::json!({
                "type": "tool_use",
                "id": inv.id,
                "name": inv.name,
                "input": inv.arguments,
            }));
        }
        if blocks.is_empty() {
            blocks.push(serde_json::json!({"type":"text","text":full}));
        }
        let sr = if invocations.is_empty() {
            "end_turn"
        } else {
            "tool_use"
        };
        (blocks, sr)
    } else {
        (
            vec![serde_json::json!({"type": "text", "text": full})],
            "end_turn",
        )
    };
    let body = serde_json::json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": model_id,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "usage": {"input_tokens": estimate_tokens(&prompt), "output_tokens": estimate_tokens(&full)},
    });
    // P3-3：Anthropic 非流式入账
    state
        .ledger
        .record(UsageRecord {
            ts: now_secs(),
            model: model_id.clone(),
            key_id: ledger::key_id(&headers),
            prompt_tokens: estimate_tokens(&prompt),
            completion_tokens: estimate_tokens(&full),
            latency_ms: 0,
            status: 200,
            stream: false,
            cached: false,
        })
        .await;
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
    body: axum::body::Bytes,
) -> Response {
    let req: anth::MessagesRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return AppError::BadRequest(format!("请求体解析失败: {e}")).into_anthropic_response()
        }
    };
    count_tokens_inner(state, headers, req)
        .await
        .unwrap_or_else(|e| e.into_anthropic_response())
}

async fn count_tokens_inner(
    state: SharedState,
    headers: HeaderMap,
    req: anth::MessagesRequest,
) -> AppResult<Response> {
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
    Ok(Json(serde_json::json!({ "input_tokens": tokens })).into_response())
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
