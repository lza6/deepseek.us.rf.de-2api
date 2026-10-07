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
    /// v2.0.0：工具定义（Anthropic 格式：name/description/input_schema）。
    #[serde(default)]
    pub tools: Option<Vec<AnthropicTool>>,
    /// "auto"/"any"/{"type":"tool","name":..}
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
}

/// Anthropic 工具声明。
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: Option<serde_json::Value>,
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
    // ── v2.0.0 工具块字段（`tool_use` / `tool_result`）──
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// `tool_result` 块的结果内容（Anthropic 规范用 `content`，可为字符串或块数组）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
}

impl TextBlock {
    /// 提取该块的可读文本内容（覆盖 `text` 与 `tool_result.content`）。
    pub fn readable(&self) -> Option<String> {
        if let Some(t) = &self.text {
            if !t.is_empty() {
                return Some(t.clone());
            }
        }
        match &self.content {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Array(arr)) => {
                let joined: Vec<String> = arr
                    .iter()
                    .filter_map(|b| {
                        b.get("text")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string())
                    })
                    .collect();
                if joined.is_empty() {
                    None
                } else {
                    Some(joined.join("\n"))
                }
            }
            _ => None,
        }
    }
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
                .filter_map(|b| b.readable())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }

    /// 消息是否为空（无文本且无工具块）。
    pub fn is_empty(&self) -> bool {
        match &self.content {
            ContentField::Text(s) => s.trim().is_empty(),
            ContentField::Blocks(bs) => {
                bs.is_empty()
                    || bs.iter().all(|b| {
                        b.readable().map(|t| t.trim().is_empty()).unwrap_or(true)
                            && b.kind
                                .as_deref()
                                .map(|k| k != "tool_use" && k != "tool_result")
                                .unwrap_or(true)
                    })
            }
        }
    }

    /// 是否是结构化块（非纯文本）。
    pub fn has_blocks(&self) -> bool {
        matches!(self.content, ContentField::Blocks(_))
    }

    /// 是否含 `tool_result` 块。
    pub fn has_tool_result(&self) -> bool {
        match &self.content {
            ContentField::Text(_) => false,
            ContentField::Blocks(bs) => bs.iter().any(|b| b.kind.as_deref() == Some("tool_result")),
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
///
/// **v2.0.0 工具调用**：`tool_use`（助手调用）与 `tool_result`（用户回传结果）
/// 块会被渲染进转录，使模型看到「调用了什么、返回了什么」。
pub fn messages_to_prompt(req: &MessagesRequest) -> Result<String, String> {
    if req.messages.is_empty() {
        return Err("messages 不能为空".into());
    }
    let system = req
        .system
        .as_ref()
        .map(|s| s.text())
        .filter(|s| !s.trim().is_empty());

    let turns: Vec<&AnthropicMessage> = req.messages.iter().filter(|m| !m.is_empty()).collect();
    if turns.is_empty() {
        return Err("缺少 user 消息".into());
    }
    if !turns
        .iter()
        .any(|m| m.role == "user" || m.has_tool_result())
    {
        return Err("缺少 user 消息".into());
    }

    let single_plain = turns.len() == 1 && !turns[0].has_blocks();
    let history = if single_plain {
        turns[0].text()
    } else {
        turns
            .iter()
            .map(|m| render_turn(m))
            .collect::<Vec<_>>()
            .join("\n")
    };

    Ok(match system {
        Some(s) => format!("{s}\n\n{history}"),
        None => history,
    })
}

/// 渲染单条 Anthropic 消息（含 tool_use / tool_result 块）。
fn render_turn(m: &AnthropicMessage) -> String {
    match &m.content {
        ContentField::Text(s) => format!("{}: {}", m.role, s),
        ContentField::Blocks(bs) => {
            let mut parts: Vec<String> = Vec::new();
            // 纯文本块（排除 tool_result，其内容单独渲染）
            let text: Vec<String> = bs
                .iter()
                .filter(|b| b.kind.as_deref() != Some("tool_result"))
                .filter_map(|b| b.text.clone())
                .collect();
            if !text.is_empty() {
                parts.push(format!("{}: {}", m.role, text.join("\n")));
            }
            for b in bs {
                match b.kind.as_deref() {
                    Some("tool_use") => {
                        let name = b.name.as_deref().unwrap_or("");
                        let input = b.input.clone().unwrap_or(serde_json::json!({}));
                        parts.push(format!(
                            "```tool\n{{\"name\":\"{name}\",\"arguments\":{input}}}\n```"
                        ));
                    }
                    Some("tool_result") => {
                        let id = b.tool_use_id.as_deref().unwrap_or("");
                        // Anthropic 规范：结果在 `content` 字段（可为字符串或块数组）
                        let body = b.readable().unwrap_or_default();
                        parts.push(format!("tool_result(id={id}): {body}"));
                    }
                    _ => {}
                }
            }
            parts.join("\n")
        }
    }
}

/// 把 Anthropic 工具定义转换为 `tools::ToolDef`。
pub fn tool_defs(req: &MessagesRequest) -> Vec<crate::tools::ToolDef> {
    req.tools
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|t| crate::tools::ToolDef {
            name: t.name.clone(),
            description: t.description.clone(),
            parameters: t.input_schema.clone(),
        })
        .collect()
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

/// 内容块。文本块 `{type:"text",text}`；工具块 `{type:"tool_use",id,name,input}`。
#[derive(Debug, Serialize)]
pub struct ContentBlock {
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    // tool_use 字段
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
}

impl ContentBlock {
    /// 文本块构造。
    pub fn text(text: impl Into<String>) -> Self {
        ContentBlock {
            kind: "text",
            text: Some(text.into()),
            id: None,
            name: None,
            input: None,
        }
    }

    /// 工具调用块构造（v2.0.0）。
    pub fn tool_use(
        id: impl Into<String>,
        name: impl Into<String>,
        input: serde_json::Value,
    ) -> Self {
        ContentBlock {
            kind: "tool_use",
            text: None,
            id: Some(id.into()),
            name: Some(name.into()),
            input: Some(input),
        }
    }
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

/// v2.0.0：工具参数增量 `{type:"input_json_delta",partial_json}`。
#[derive(Debug, Serialize)]
pub struct InputJsonDelta {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub partial_json: String,
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
    /// L7：Anthropic 规范中 `message_delta.usage` **只含** `output_tokens`
    /// （`input_tokens` 在 `message_start` 里给）。此前误带 `input_tokens: 0`，
    /// 会让合并 usage 的客户端把输入 token 归零。
    pub usage: OutputUsage,
}

/// L7：`message_delta` 的 usage（仅 output_tokens）。
#[derive(Debug, Serialize)]
pub struct OutputUsage {
    pub output_tokens: u32,
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
            tools: None,
            tool_choice: None,
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
            tools: None,
            tool_choice: None,
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
                id: None,
                name: None,
                input: None,
                tool_use_id: None,
                content: None,
            }])),
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: ContentField::Blocks(vec![TextBlock {
                    kind: Some("text".into()),
                    text: Some("hello".into()),
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: None,
                    content: None,
                }]),
            }],
            max_tokens: None,
            stream: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };
        // v2.0.0：结构化块走转录渲染（带角色标注，以支持 tool_use/tool_result）
        let p = messages_to_prompt(&req).unwrap();
        assert!(p.starts_with("sys"), "{p}");
        assert!(p.contains("hello"), "{p}");
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
            tools: None,
            tool_choice: None,
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
            tools: None,
            tool_choice: None,
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
            tools: None,
            tool_choice: None,
        };
        assert!(messages_to_prompt(&req).is_err());
    }

    // ── v2.0.0 工具调用（Anthropic 侧） ────────────────────────

    #[test]
    fn anthropic_tools_deserialized() {
        let raw = r#"{
            "model":"deepseek-es","max_tokens":100,
            "messages":[{"role":"user","content":"hi"}],
            "tools":[{"name":"get_weather","description":"查天气","input_schema":{"type":"object"}}]
        }"#;
        let req: MessagesRequest = serde_json::from_str(raw).unwrap();
        let defs = tool_defs(&req);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "get_weather");
        assert!(defs[0].parameters.is_some());
    }

    #[test]
    fn anthropic_tool_use_block_rendered() {
        // 助手的历史 tool_use 块应渲染为 ```tool 块
        let req = MessagesRequest {
            model: None,
            system: None,
            messages: vec![
                am("user", "北京天气？"),
                AnthropicMessage {
                    role: "assistant".into(),
                    content: ContentField::Blocks(vec![TextBlock {
                        kind: Some("tool_use".into()),
                        text: None,
                        id: Some("toolu_1".into()),
                        name: Some("get_weather".into()),
                        input: Some(serde_json::json!({"city":"北京"})),
                        tool_use_id: None,
                        content: None,
                    }]),
                },
                AnthropicMessage {
                    role: "user".into(),
                    content: ContentField::Blocks(vec![TextBlock {
                        kind: Some("tool_result".into()),
                        text: None,
                        id: None,
                        name: None,
                        input: None,
                        tool_use_id: Some("toolu_1".into()),
                        content: Some(serde_json::json!("晴 25°C")),
                    }]),
                },
            ],
            max_tokens: None,
            stream: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };
        let p = messages_to_prompt(&req).unwrap();
        assert!(p.contains("get_weather"), "工具名应出现: {p}");
        assert!(p.contains("北京"), "工具参数应出现: {p}");
        assert!(p.contains("晴 25°C"), "工具结果应出现: {p}");
    }

    #[test]
    fn anthropic_tool_result_only_is_valid() {
        // 只有 tool_result（无 user 文本）也应可解析
        let req = MessagesRequest {
            model: None,
            system: None,
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: ContentField::Blocks(vec![TextBlock {
                    kind: Some("tool_result".into()),
                    text: None,
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: Some("toolu_1".into()),
                    content: Some(serde_json::json!("结果")),
                }]),
            }],
            max_tokens: None,
            stream: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };
        assert!(messages_to_prompt(&req).is_ok());
    }

    #[test]
    fn anthropic_tool_use_content_block_serialization() {
        let b = ContentBlock::tool_use("toolu_1", "get_weather", serde_json::json!({"city":"x"}));
        let s = serde_json::to_string(&b).unwrap();
        assert!(s.contains("\"type\":\"tool_use\""), "{s}");
        assert!(s.contains("\"name\":\"get_weather\""), "{s}");
        assert!(s.contains("\"input\""), "{s}");
        assert!(!s.contains("\"text\""), "tool_use 不应含 text 字段: {s}");
    }

    #[test]
    fn anthropic_input_json_delta_serialization() {
        let d = InputJsonDelta {
            kind: "input_json_delta",
            partial_json: "{\"a\":1}".into(),
        };
        let s = serde_json::to_string(&d).unwrap();
        assert!(s.contains("input_json_delta"), "{s}");
        assert!(s.contains("partial_json"), "{s}");
    }

    // ── tool_result 的 content 字段（Anthropic 规范，真实 bug 回归） ──

    #[test]
    fn tool_result_content_string_readable() {
        let b = TextBlock {
            kind: Some("tool_result".into()),
            text: None,
            id: None,
            name: None,
            input: None,
            tool_use_id: Some("toolu_1".into()),
            content: Some(serde_json::json!("晴 25°C")),
        };
        assert_eq!(b.readable().as_deref(), Some("晴 25°C"));
    }

    #[test]
    fn tool_result_content_blocks_readable() {
        let b = TextBlock {
            kind: Some("tool_result".into()),
            text: None,
            id: None,
            name: None,
            input: None,
            tool_use_id: Some("toolu_1".into()),
            content: Some(serde_json::json!([{"type":"text","text":"多云 22 度"}])),
        };
        assert_eq!(b.readable().as_deref(), Some("多云 22 度"));
    }

    #[test]
    fn tool_result_content_reaches_prompt() {
        // 端到端：tool_result.content（非 text）必须进入 prompt
        let req = MessagesRequest {
            model: None,
            system: None,
            messages: vec![
                am("user", "上海天气？"),
                AnthropicMessage {
                    role: "user".into(),
                    content: ContentField::Blocks(vec![TextBlock {
                        kind: Some("tool_result".into()),
                        text: None,
                        id: None,
                        name: None,
                        input: None,
                        tool_use_id: Some("toolu_1".into()),
                        content: Some(serde_json::json!("多云 22 度")),
                    }]),
                },
            ],
            max_tokens: None,
            stream: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };
        let p = messages_to_prompt(&req).unwrap();
        assert!(
            p.contains("多云 22 度"),
            "tool_result.content 未进入 prompt: {p}"
        );
    }
}
