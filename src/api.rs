//! HTTP 路由与处理器：OpenAI + Anthropic 兼容端点 + 管理端点。

use crate::auth::check_auth;
use crate::config::Config;
use crate::errors::{AppError, AppResult};
use crate::models::{self, DEFAULT_MODEL};
use crate::protocol::anthropic::{self as anth};
use crate::protocol::openai::{self as oai};
use crate::session::SessionStore;
use crate::upstream::UpstreamClient;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use std::convert::Infallible;
use std::sync::Arc;

pub struct AppState {
    pub cfg: Config,
    pub upstream: Arc<UpstreamClient>,
    pub sessions: SessionStore,
}

pub type SharedState = Arc<AppState>;

pub fn build_router(state: SharedState) -> Router {
    let mut router = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(list_models))
        .route("/v1/chat/completions", post(openai_chat))
        .route("/v1/messages", post(anthropic_messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
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
    let model_id = req
        .model
        .clone()
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let meta = models::resolve_model(&model_id);
    let prompt = oai::messages_to_prompt(&req)?;
    let stream = req.stream.unwrap_or(false);

    let session_key = req.user.clone().or_else(|| {
        headers
            .get("x-session-id")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
    });
    let (_sid, conv_uuid) = state.sessions.get_or_create(session_key.as_deref());

    // 确保认证
    state.upstream.ensure_authed().await?;

    if stream {
        let (events, id) = start_stream(&state, &prompt, &conv_uuid, &model_id, &meta).await?;
        let model = model_id.clone();
        let out = events
            .map(move |item| -> Result<Event, Infallible> {
                let ev = match item {
                    Ok(t) => t,
                    Err(e) => oai::Translated::Error(e.to_string()),
                };
                match ev {
                    oai::Translated::Delta(text) => {
                        let chunk = oai::content_chunk(&id, &model, &text);
                        Ok(Event::default().data(serde_json::to_string(&chunk).unwrap()))
                    }
                    oai::Translated::Done => {
                        let stop = oai::stop_chunk(&id, &model);
                        Ok(Event::default().data(serde_json::to_string(&stop).unwrap()))
                    }
                    oai::Translated::Error(e) => {
                        let err =
                            serde_json::json!({"error":{"message":e,"type":"upstream_error"}});
                        Ok(Event::default().data(serde_json::to_string(&err).unwrap()))
                    }
                }
            })
            .chain(futures::stream::once(async {
                Ok(Event::default().data("[DONE]"))
            }));
        return Ok(Sse::new(out).into_response());
    }

    // 非流式：聚合
    let (mut events, id) = start_stream(&state, &prompt, &conv_uuid, &model_id, &meta).await?;
    let mut full = String::new();
    while let Some(item) = events.next().await {
        match item {
            Ok(oai::Translated::Delta(t)) => full.push_str(&t),
            Ok(oai::Translated::Done) => break,
            Ok(oai::Translated::Error(e)) => {
                if e == "__TS_REQUIRED__" {
                    return Err(AppError::TsRequired);
                }
                return Err(AppError::UpstreamStream(e));
            }
            Err(e) => return Err(e),
        }
    }
    // usage 粗略估算（字符数/4）
    let approx = (full.chars().count() as u32).div_ceil(4);
    let completion = oai::ChatCompletion {
        id,
        object: "chat.completion".into(),
        created: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        model: model_id,
        choices: vec![oai::CompletionChoice {
            index: 0,
            message: oai::AssistantMessage {
                role: "assistant".into(),
                content: full,
            },
            finish_reason: "stop".into(),
        }],
        usage: oai::Usage {
            prompt_tokens: (prompt.chars().count() as u32).div_ceil(4),
            completion_tokens: approx,
            total_tokens: (prompt.chars().count() as u32).div_ceil(4) + approx,
        },
    };
    Ok(Json(completion).into_response())
}

/// 启动一次上游流：cache_message → stream_chat，返回翻译后的事件流。
async fn start_stream(
    state: &SharedState,
    prompt: &str,
    conv_uuid: &str,
    _model_id: &str,
    _meta: &models::ModelMeta,
) -> AppResult<(
    std::pin::Pin<Box<dyn futures::Stream<Item = AppResult<oai::Translated>> + Send>>,
    String,
)> {
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
    let meta = models::resolve_model(&model_id);
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
        "usage": {"input_tokens": 0, "output_tokens": (full.chars().count() as u32).div_ceil(4)},
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
    let tokens = (text.chars().count() as u32).div_ceil(4).max(1);
    Ok(Json(serde_json::json!({ "input_tokens": tokens })))
}

/// 会话 TTL 常量（供 main 使用）。
pub const SESSION_TTL_SECS: u64 = 3600;
