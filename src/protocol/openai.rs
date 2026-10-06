//! OpenAI Chat Completions 兼容：请求解析 + SSE chunk 生成。
//!
//! 映射（真实 E2E 确认）：
//!   上游 `message_start` {message_id}       → 生成 chatcmpl id（首个 chunk）
//!   上游 `message`       {delta}            → choices[0].delta.content
//!   上游 `done`          {finished}         → finish_reason:"stop" + [DONE]
//!   上游 `error`         {error,ts_required}→ 抛错

use crate::errors::{AppError, AppResult};
use crate::upstream::SseEvent;
use serde::{Deserialize, Serialize};

// ── 请求 ────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct ChatRequest {
    pub model: Option<String>,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: Option<Content>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<Part>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Part {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

impl Message {
    pub fn text(&self) -> String {
        match &self.content {
            None => String::new(),
            Some(Content::Text(s)) => s.clone(),
            Some(Content::Parts(parts)) => parts
                .iter()
                .filter_map(|p| p.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// 把 OpenAI messages 转成上游单条文本提示。
///
/// 上游（AIPKit bot）只接收单条消息，历史由服务端按 conversation_uuid 维护。
/// 因此这里把 system + 最近 user 内容拼成 prompt；若含多轮历史，
/// 保留最近一轮 user 文本（其余由会话 UUID 承接）。
pub fn messages_to_prompt(req: &ChatRequest) -> AppResult<String> {
    if req.messages.is_empty() {
        return Err(AppError::BadRequest("messages 不能为空".into()));
    }
    // 收集 system 前缀
    let system: Vec<String> = req
        .messages
        .iter()
        .filter(|m| m.role == "system")
        .map(|m| m.text())
        .filter(|s| !s.trim().is_empty())
        .collect();

    // 最后一条 user
    let last_user = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.text())
        .ok_or_else(|| AppError::BadRequest("缺少 user 消息".into()))?;

    if last_user.trim().is_empty() {
        return Err(AppError::BadRequest("user 消息内容为空".into()));
    }

    if system.is_empty() {
        Ok(last_user)
    } else {
        Ok(format!("{}\n\n{}", system.join("\n"), last_user))
    }
}

// ── 响应 ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ChunkChoice {
    pub index: u32,
    pub delta: Delta,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Delta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChatChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
}

#[derive(Debug, Serialize)]
pub struct CompletionChoice {
    pub index: u32,
    pub message: AssistantMessage,
    pub finish_reason: String,
}

#[derive(Debug, Serialize)]
pub struct AssistantMessage {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Serialize)]
pub struct ChatCompletion {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<CompletionChoice>,
    pub usage: Usage,
}

#[derive(Debug, Default, Serialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 生成首个 chunk（role 声明）。
pub fn first_chunk(id: &str, model: &str) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".into(),
        created: now_secs(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta {
                role: Some("assistant".into()),
                content: None,
            },
            finish_reason: None,
        }],
    }
}

pub fn content_chunk(id: &str, model: &str, content: &str) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".into(),
        created: now_secs(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta {
                role: None,
                content: Some(content.to_string()),
            },
            finish_reason: None,
        }],
    }
}

pub fn stop_chunk(id: &str, model: &str) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".into(),
        created: now_secs(),
        model: model.to_string(),
        choices: vec![ChunkChoice {
            index: 0,
            delta: Delta::default(),
            finish_reason: Some("stop".into()),
        }],
    }
}

/// 把上游 SSE 事件翻译为「文本增量」或「结束信号」或「错误」。
#[derive(Debug, PartialEq)]
pub enum Translated {
    /// 文本增量
    Delta(String),
    /// 流结束
    Done,
    /// 错误
    Error(String),
}

/// 解析上游 SSE 事件 → 翻译结果。非文本事件返回 None。
pub fn translate_event(ev: &SseEvent) -> Option<Translated> {
    match ev.event.as_str() {
        "message" => {
            let v: serde_json::Value = match serde_json::from_str(&ev.data) {
                Ok(v) => v,
                Err(_) => return None,
            };
            if let Some(d) = v.get("delta").and_then(|d| d.as_str()) {
                if d.is_empty() {
                    return None;
                }
                return Some(Translated::Delta(d.to_string()));
            }
            None
        }
        "done" => Some(Translated::Done),
        "error" => {
            let v: serde_json::Value = serde_json::from_str(&ev.data).unwrap_or_default();
            let msg = v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("上游流错误")
                .to_string();
            if v.get("ts_required")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                return Some(Translated::Error("__TS_REQUIRED__".into()));
            }
            Some(Translated::Error(msg))
        }
        "__stream_error__" => Some(Translated::Error(ev.data.clone())),
        // message_start / status / citations / grounding_metadata / display_form_event / warning
        // 本站（provider=DeepSeek）不产生文本，对 OpenAI 输出忽略
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: Some(Content::Text(text.into())),
        }
    }

    #[test]
    fn prompt_last_user_only() {
        let req = ChatRequest {
            model: None,
            messages: vec![msg("user", "hello")],
            stream: None,
            temperature: None,
            max_tokens: None,
            user: None,
        };
        assert_eq!(messages_to_prompt(&req).unwrap(), "hello");
    }

    #[test]
    fn prompt_with_system() {
        let req = ChatRequest {
            model: None,
            messages: vec![msg("system", "be brief"), msg("user", "hi")],
            stream: None,
            temperature: None,
            max_tokens: None,
            user: None,
        };
        assert_eq!(messages_to_prompt(&req).unwrap(), "be brief\n\nhi");
    }

    #[test]
    fn prompt_picks_last_user() {
        let req = ChatRequest {
            model: None,
            messages: vec![
                msg("user", "first"),
                msg("assistant", "ok"),
                msg("user", "second"),
            ],
            stream: None,
            temperature: None,
            max_tokens: None,
            user: None,
        };
        assert_eq!(messages_to_prompt(&req).unwrap(), "second");
    }

    #[test]
    fn prompt_empty_messages_err() {
        let req = ChatRequest {
            model: None,
            messages: vec![],
            stream: None,
            temperature: None,
            max_tokens: None,
            user: None,
        };
        assert!(messages_to_prompt(&req).is_err());
    }

    #[test]
    fn translate_delta() {
        let ev = SseEvent {
            event: "message".into(),
            data: r#"{"delta":"hi"}"#.into(),
        };
        assert_eq!(translate_event(&ev), Some(Translated::Delta("hi".into())));
    }

    #[test]
    fn translate_empty_delta_ignored() {
        let ev = SseEvent {
            event: "message".into(),
            data: r#"{"delta":""}"#.into(),
        };
        assert_eq!(translate_event(&ev), None);
    }

    #[test]
    fn translate_done() {
        let ev = SseEvent {
            event: "done".into(),
            data: r#"{"finished":true}"#.into(),
        };
        assert_eq!(translate_event(&ev), Some(Translated::Done));
    }

    #[test]
    fn translate_ts_required() {
        let ev = SseEvent {
            event: "error".into(),
            data: r#"{"error":"Sicherheitspruefung erforderlich.","ts_required":true}"#.into(),
        };
        assert_eq!(
            translate_event(&ev),
            Some(Translated::Error("__TS_REQUIRED__".into()))
        );
    }

    #[test]
    fn translate_message_start_ignored() {
        let ev = SseEvent {
            event: "message_start".into(),
            data: r#"{"message_id":"x"}"#.into(),
        };
        assert_eq!(translate_event(&ev), None);
    }

    #[test]
    fn chunk_serialization() {
        let c = content_chunk("id", "m", "x");
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"delta\":{\"content\":\"x\"}"));
        assert!(s.contains("chat.completion.chunk"));
        assert!(!s.contains("role"));
    }
}
