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
///
/// **H2 修复**（与 OpenAI 侧一致）：Anthropic 客户端同样每轮发送完整历史。
/// 多轮时渲染完整转录（带角色标注），单轮时保持原样。
pub fn messages_to_prompt(req: &MessagesRequest) -> Result<String, String> {
    if req.messages.is_empty() {
        return Err("messages 不能为空".into());
    }
    let system = req
        .system
        .as_ref()
        .map(|s| s.text())
        .filter(|s| !s.trim().is_empty());

    let turns: Vec<&AnthropicMessage> = req
        .messages
        .iter()
        .filter(|m| !m.text().trim().is_empty())
        .collect();
    if turns.is_empty() || !turns.iter().any(|m| m.role == "user") {
        return Err("缺少 user 消息".into());
    }

    let history = if turns.len() == 1 {
        turns[0].text()
    } else {
        turns
            .iter()
            .map(|m| format!("{}: {}", m.role, m.text()))
            .collect::<Vec<_>>()
            .join("\n")
    };

    Ok(match system {
        Some(s) => format!("{s}\n\n{history}"),
        None => history,
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

    // ── H2 回归：Anthropic 多轮历史必须保留 ──────────────────────

    #[test]
    fn anthropic_multi_turn_history_preserved() {
        let req = MessagesRequest {
            model: None,
            system: None,
            messages: vec![
                am("user", "我叫小明"),
                am("assistant", "你好"),
                am("user", "我叫什么"),
            ],
            max_tokens: None,
            stream: None,
            temperature: None,
        };
        let p = messages_to_prompt(&req).unwrap();
        assert!(
            p.contains("我叫小明") && p.contains("你好") && p.contains("我叫什么"),
            "{p}"
        );
    }

    #[test]
    fn anthropic_single_turn_unchanged() {
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
    fn anthropic_missing_user_errors() {
        let req = MessagesRequest {
            model: None,
            system: None,
            messages: vec![am("assistant", "hi")],
            max_tokens: None,
            stream: None,
            temperature: None,
        };
        assert!(messages_to_prompt(&req).is_err());
    }
}
