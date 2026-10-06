//! Anthropic Messages API 兼容（供 Claude Code 使用）。
//!
//! 映射：
//!   /v1/messages            → 上游聊天
//!   /v1/messages/count_tokens → 粗略 token 估算

use serde::{Deserialize, Serialize};

// ── 请求 ────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct MessagesRequest {
    pub model: Option<String>,
    #[serde(default)]
    pub system: Option<SystemField>,
    pub messages: Vec<AnthropicMessage>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub temperature: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum SystemField {
    Text(String),
    Blocks(Vec<TextBlock>),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TextBlock {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: ContentField,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum ContentField {
    Text(String),
    Blocks(Vec<TextBlock>),
}

impl AnthropicMessage {
    pub fn text(&self) -> String {
        match &self.content {
            ContentField::Text(s) => s.clone(),
            ContentField::Blocks(bs) => bs
                .iter()
                .filter_map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

impl SystemField {
    pub fn text(&self) -> String {
        match self {
            SystemField::Text(s) => s.clone(),
            SystemField::Blocks(bs) => bs
                .iter()
                .filter_map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// 把 Anthropic messages 转成上游单条提示。
pub fn messages_to_prompt(req: &MessagesRequest) -> Result<String, String> {
    if req.messages.is_empty() {
        return Err("messages 不能为空".into());
    }
    let system = req
        .system
        .as_ref()
        .map(|s| s.text())
        .filter(|s| !s.trim().is_empty());
    let last_user = req
        .messages
        .iter()
        .rev()
        .find(|m| m.role == "user")
        .map(|m| m.text())
        .ok_or_else(|| "缺少 user 消息".to_string())?;
    if last_user.trim().is_empty() {
        return Err("user 消息内容为空".into());
    }
    Ok(match system {
        Some(s) => format!("{s}\n\n{last_user}"),
        None => last_user,
    })
}

// ── 响应（SSE 事件）──────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct MessageStartEvent {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub message: MessageBody,
}

#[derive(Debug, Serialize)]
pub struct MessageBody {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub role: &'static str,
    pub model: String,
    pub content: Vec<serde_json::Value>,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
    pub usage: AnthropicUsage,
}

#[derive(Debug, Serialize)]
pub struct AnthropicUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[derive(Debug, Serialize)]
pub struct ContentBlockStart {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub index: u32,
    pub content_block: ContentBlock,
}

#[derive(Debug, Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct ContentBlockDelta {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub index: u32,
    pub delta: TextDelta,
}

#[derive(Debug, Serialize)]
pub struct TextDelta {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct ContentBlockStop {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub index: u32,
}

#[derive(Debug, Serialize)]
pub struct MessageDelta {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub delta: DeltaStop,
    pub usage: AnthropicUsage,
}

#[derive(Debug, Serialize)]
pub struct DeltaStop {
    pub stop_reason: String,
    pub stop_sequence: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MessageStop {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

pub const SSE_EVENT_MESSAGE_START: &str = "message_start";
pub const SSE_EVENT_CONTENT_BLOCK_START: &str = "content_block_start";
pub const SSE_EVENT_CONTENT_BLOCK_DELTA: &str = "content_block_delta";
pub const SSE_EVENT_CONTENT_BLOCK_STOP: &str = "content_block_stop";
pub const SSE_EVENT_MESSAGE_DELTA: &str = "message_delta";
pub const SSE_EVENT_MESSAGE_STOP: &str = "message_stop";

#[cfg(test)]
mod tests {
    use super::*;

    fn am(role: &str, text: &str) -> AnthropicMessage {
        AnthropicMessage {
            role: role.into(),
            content: ContentField::Text(text.into()),
        }
    }

    #[test]
    fn prompt_with_system() {
        let req = MessagesRequest {
            model: None,
            system: Some(SystemField::Text("be brief".into())),
            messages: vec![am("user", "hi")],
            max_tokens: None,
            stream: None,
            temperature: None,
        };
        assert_eq!(messages_to_prompt(&req).unwrap(), "be brief\n\nhi");
    }

    #[test]
    fn prompt_no_system() {
        let req = MessagesRequest {
            model: None,
            system: None,
            messages: vec![am("user", "hi")],
            max_tokens: None,
            stream: None,
            temperature: None,
        };
        assert_eq!(messages_to_prompt(&req).unwrap(), "hi");
    }

    #[test]
    fn prompt_blocks() {
        let req = MessagesRequest {
            model: None,
            system: Some(SystemField::Blocks(vec![TextBlock {
                kind: Some("text".into()),
                text: Some("sys".into()),
            }])),
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: ContentField::Blocks(vec![TextBlock {
                    kind: Some("text".into()),
                    text: Some("hello".into()),
                }]),
            }],
            max_tokens: None,
            stream: None,
            temperature: None,
        };
        assert_eq!(messages_to_prompt(&req).unwrap(), "sys\n\nhello");
    }
}
