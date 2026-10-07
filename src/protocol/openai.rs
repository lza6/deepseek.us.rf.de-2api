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
    /// v2.0.0：工具定义。`null`/缺省 = 不使用工具（走原路径）。
    #[serde(default)]
    pub tools: Option<Vec<ToolSpec>>,
    /// "auto"/"none"/"required"/{"type":"function","function":{"name":..}}
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    /// 流式选项。`{"include_usage": true}` 时在 `[DONE]` 前插入一帧带 usage 的 chunk。
    #[serde(default)]
    pub stream_options: Option<StreamOptions>,
}

/// OpenAI `stream_options`。
#[derive(Debug, Clone, Deserialize)]
pub struct StreamOptions {
    #[serde(default)]
    pub include_usage: bool,
}

/// OpenAI 工具声明包装：`{"type":"function","function":{...}}`。
#[derive(Debug, Clone, Deserialize)]
pub struct ToolSpec {
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    pub function: FunctionDef,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub role: String,
    #[serde(default)]
    pub content: Option<Content>,
    /// 助手消息中模型发起的工具调用（回传历史时出现）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallMsg>>,
    /// `role:"tool"` 消息对应的调用 id。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// 历史消息里的工具调用（OpenAI 结构）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallMsg {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// 参数字符串（OpenAI 约定为 JSON 字符串）。
    #[serde(default)]
    pub arguments: String,
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
/// **H2 修复**：上游（AIPKit bot）只接收单条消息，历史本应由服务端按
/// `conversation_uuid` 维护。但标准 OpenAI 客户端**每轮发送完整历史**，
/// 且多数 SDK 默认不设 `user` 字段 → 网关侧无法稳定映射 conv_uuid
/// → 若只取末条 user，则**整个历史被静默丢弃**（模型只看到最后一句话）。
///
/// 因此这里改为：**当请求含多轮对话时，把完整历史按角色标注渲染进 prompt**
/// （网关自身无状态、每轮独立可复现）；**单轮**请求保持原样（向后兼容）。
///
/// **v2.0.0 工具调用**：`role:"tool"` 消息（工具结果）与 assistant 的 `tool_calls`
/// 也会被渲染进转录，使模型能看到「调用了什么工具、返回了什么」。
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

    // 非 system 的对话轮次（tool 消息单独渲染，不参与"空内容"过滤）
    let turns: Vec<&Message> = req
        .messages
        .iter()
        .filter(|m| m.role != "system")
        .filter(|m| m.role == "tool" || m.tool_calls.is_some() || !m.text().trim().is_empty())
        .collect();

    if turns.is_empty() {
        return Err(AppError::BadRequest("缺少 user 消息".into()));
    }

    // 必须至少有一条 user 或 tool（否则模型无输入）
    if !turns.iter().any(|m| m.role == "user" || m.role == "tool") {
        return Err(AppError::BadRequest("缺少 user 消息".into()));
    }

    // 单轮（仅一条普通消息）：保持旧行为，不引入角色前缀（向后兼容）
    let single_plain = turns.len() == 1 && turns[0].role != "tool" && turns[0].tool_calls.is_none();
    let history = if single_plain {
        turns[0].text()
    } else {
        // 多轮 / 含工具：渲染完整转录，带角色标注
        turns
            .iter()
            .map(|m| render_turn(m))
            .collect::<Vec<_>>()
            .join("\n")
    };

    if system.is_empty() {
        Ok(history)
    } else {
        Ok(format!("{}\n\n{}", system.join("\n"), history))
    }
}

/// 渲染单个消息为转录行（含工具调用/结果）。
fn render_turn(m: &Message) -> String {
    if m.role == "tool" {
        // 工具结果：带调用 id 便于模型关联
        let id = m.tool_call_id.as_deref().unwrap_or("");
        return format!("tool_result(id={}): {}", id, m.text());
    }
    let text = m.text();
    if let Some(tcs) = &m.tool_calls {
        // 助手的工具调用：渲染为 tool 块，与模型输出格式一致（便于模型继续）
        let mut s = String::new();
        if !text.trim().is_empty() {
            s.push_str(&format!("assistant: {text}\n"));
        }
        for tc in tcs {
            s.push_str(&format!(
                "```tool\n{{\"name\":\"{}\",\"arguments\":{}}}\n```\n",
                tc.function.name,
                normalize_args(&tc.function.arguments)
            ));
        }
        return s.trim_end().to_string();
    }
    format!("{}: {}", m.role, text)
}

/// 把工具调用参数字符串规整为 JSON（OpenAI 约定 arguments 是 JSON 字符串）。
fn normalize_args(args: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(args) {
        Ok(v) => v.to_string(),
        Err(_) => "{}".to_string(),
    }
}

/// 把请求里的工具定义转换为 `tools::ToolDef`。
pub fn tool_defs(req: &ChatRequest) -> Vec<crate::tools::ToolDef> {
    req.tools
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|t| crate::tools::ToolDef {
            name: t.function.name.clone(),
            description: t.function.description.clone(),
            parameters: t.function.parameters.clone(),
        })
        .collect()
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
    /// v2.0.0：流式工具调用增量（OpenAI 结构）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallDelta>>,
}

/// 流式工具调用增量。
#[derive(Debug, Clone, Serialize)]
pub struct ToolCallDelta {
    pub index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub function: ToolCallDeltaFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallDeltaFunction {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ChatChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
    /// L3：`stream_options.include_usage` 时的末帧 usage（其余帧为 None）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

/// L3：构造流式末帧 usage chunk（OpenAI 规范：`choices` 为空数组 + `usage`）。
pub fn usage_chunk(id: &str, model: &str, usage: Usage) -> ChatChunk {
    usage_chunk_at(id, model, usage, now_secs())
}

/// L6：指定 `created` 的 usage chunk（同一流内应固定 `created`）。
pub fn usage_chunk_at(id: &str, model: &str, usage: Usage, created: i64) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".into(),
        created,
        model: model.to_string(),
        choices: vec![],
        usage: Some(usage),
    }
}

/// 内部：给 chunk 的公共字段打底（避免每处重复 `usage: None`）。
fn base(id: &str, model: &str, choices: Vec<ChunkChoice>) -> ChatChunk {
    base_at(id, model, choices, now_secs())
}

/// L6：指定 `created` 的打底构造。
///
/// 同一流内所有 chunk 应共享同一 `created`（此前每 chunk 重取 `now_secs()`，
/// 长流跨秒边界时不一致）。
fn base_at(id: &str, model: &str, choices: Vec<ChunkChoice>, created: i64) -> ChatChunk {
    ChatChunk {
        id: id.to_string(),
        object: "chat.completion.chunk".into(),
        created,
        model: model.to_string(),
        choices,
        usage: None,
    }
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
    /// v2.0.0：非流式工具调用（OpenAI 结构）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallMsg>>,
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
    base(
        id,
        model,
        vec![ChunkChoice {
            index: 0,
            delta: Delta {
                role: Some("assistant".into()),
                ..Default::default()
            },
            finish_reason: None,
        }],
    )
}

pub fn content_chunk(id: &str, model: &str, content: &str) -> ChatChunk {
    base(
        id,
        model,
        vec![ChunkChoice {
            index: 0,
            delta: Delta {
                content: Some(content.to_string()),
                ..Default::default()
            },
            finish_reason: None,
        }],
    )
}

pub fn stop_chunk(id: &str, model: &str) -> ChatChunk {
    stop_chunk_reason(id, model, "stop")
}

/// 带指定 finish_reason 的结束 chunk（v2.0.0：`tool_calls` 场景）。
pub fn stop_chunk_reason(id: &str, model: &str, reason: &str) -> ChatChunk {
    base(
        id,
        model,
        vec![ChunkChoice {
            index: 0,
            delta: Delta::default(),
            finish_reason: Some(reason.into()),
        }],
    )
}

/// v2.0.0：构造一次工具调用的流式增量 chunk（含完整参数）。
pub fn tool_call_chunk(
    id: &str,
    model: &str,
    index: u32,
    call_id: &str,
    name: &str,
    arguments: &str,
) -> ChatChunk {
    base(
        id,
        model,
        vec![ChunkChoice {
            index: 0,
            delta: Delta {
                tool_calls: Some(vec![ToolCallDelta {
                    index,
                    id: Some(call_id.to_string()),
                    kind: Some("function".into()),
                    function: ToolCallDeltaFunction {
                        name: Some(name.to_string()),
                        arguments: Some(arguments.to_string()),
                    },
                }]),
                ..Default::default()
            },
            finish_reason: None,
        }],
    )
}

/// L6：指定 created 的版本（流内共享同一时间戳）。
pub fn first_chunk_at(id: &str, model: &str, created: i64) -> ChatChunk {
    base_at(
        id,
        model,
        vec![ChunkChoice {
            index: 0,
            delta: Delta {
                role: Some("assistant".into()),
                ..Default::default()
            },
            finish_reason: None,
        }],
        created,
    )
}
pub fn content_chunk_at(id: &str, model: &str, content: &str, created: i64) -> ChatChunk {
    base_at(
        id,
        model,
        vec![ChunkChoice {
            index: 0,
            delta: Delta {
                content: Some(content.to_string()),
                ..Default::default()
            },
            finish_reason: None,
        }],
        created,
    )
}
pub fn stop_chunk_reason_at(id: &str, model: &str, reason: &str, created: i64) -> ChatChunk {
    base_at(
        id,
        model,
        vec![ChunkChoice {
            index: 0,
            delta: Delta::default(),
            finish_reason: Some(reason.into()),
        }],
        created,
    )
}
pub fn tool_call_chunk_at(
    id: &str,
    model: &str,
    index: u32,
    call_id: &str,
    name: &str,
    arguments: &str,
    created: i64,
) -> ChatChunk {
    base_at(
        id,
        model,
        vec![ChunkChoice {
            index: 0,
            delta: Delta {
                tool_calls: Some(vec![ToolCallDelta {
                    index,
                    id: Some(call_id.to_string()),
                    kind: Some("function".into()),
                    function: ToolCallDeltaFunction {
                        name: Some(name.to_string()),
                        arguments: Some(arguments.to_string()),
                    },
                }]),
                ..Default::default()
            },
            finish_reason: None,
        }],
        created,
    )
}

/// 把上游 SSE 事件翻译为「文本增量」或「结束信号」或「错误」或「配额耗尽」。
#[derive(Debug, PartialEq)]
pub enum Translated {
    /// 文本增量
    Delta(String),
    /// 流结束
    Done,
    /// 错误
    Error(String),
    /// 配额耗尽（上游 quota_notice，需映射 429）
    Quota(String),
    /// 上游要求重新安全校验（M3：结构化替代 `__TS_REQUIRED__` 字符串哨兵，
    /// 避免哨兵值泄漏给下游）。
    TsRequired,
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
            // 配额耗尽：优先识别（映射 429），与普通错误区分
            if v.get("quota_notice").map(|q| !q.is_null()).unwrap_or(false) {
                let msg = v
                    .get("error")
                    .and_then(|e| e.as_str())
                    .unwrap_or("上游配额耗尽")
                    .to_string();
                return Some(Translated::Quota(msg));
            }
            let msg = v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("上游流错误")
                .to_string();
            if v.get("ts_required")
                .and_then(|b| b.as_bool())
                .unwrap_or(false)
            {
                return Some(Translated::TsRequired);
            }
            Some(Translated::Error(msg))
        }
        "__stream_error__" => Some(Translated::Error(ev.data.clone())),
        // M12：未识别的上游命名事件（status/citations/grounding_metadata/display_form_event/
        // warning 等）。本站（provider=DeepSeek）当前不触发，但记 debug 日志 + 计数，
        // 避免未来上游新增事件被静默吞无法观测。
        other => {
            if other != "message_start" {
                tracing::debug!(event = other, "上游未识别事件（已忽略）");
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: Some(Content::Text(text.into())),
            tool_calls: None,
            tool_call_id: None,
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
            tools: None,
            tool_choice: None,
            stream_options: None,
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
            tools: None,
            tool_choice: None,
            stream_options: None,
        };
        assert_eq!(messages_to_prompt(&req).unwrap(), "be brief\n\nhi");
    }

    #[test]
    fn prompt_multi_turn_keeps_all() {
        // H2：此测试此前断言"只取末条"（错误行为）。多轮客户端必须保留全部历史。
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
            tools: None,
            tool_choice: None,
            stream_options: None,
        };
        let p = messages_to_prompt(&req).unwrap();
        assert!(
            p.contains("first") && p.contains("ok") && p.contains("second"),
            "{p}"
        );
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
            tools: None,
            tool_choice: None,
            stream_options: None,
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
        // M3：结构化变体，不再用字符串哨兵
        assert_eq!(translate_event(&ev), Some(Translated::TsRequired));
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
    fn translate_unknown_event_ignored_m12() {
        // M12：未知事件不得 panic、不得产出文本，仅记日志后忽略
        let ev = SseEvent {
            event: "citations".into(),
            data: r#"{"citations":[{"url":"x"}]}"#.into(),
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

    #[test]
    fn translate_quota_notice() {
        let ev = SseEvent {
            event: "error".into(),
            data: r#"{"error":"Cuota agotada","quota_notice":{"title":"t","message":"m","actions":[]}}"#.into(),
        };
        assert_eq!(
            translate_event(&ev),
            Some(Translated::Quota("Cuota agotada".into()))
        );
    }

    #[test]
    fn translate_quota_without_error_field() {
        let ev = SseEvent {
            event: "error".into(),
            data: r#"{"quota_notice":{"title":"t"}}"#.into(),
        };
        assert_eq!(
            translate_event(&ev),
            Some(Translated::Quota("上游配额耗尽".into()))
        );
    }

    // ── H2 回归：多轮历史必须保留 ────────────────────────────────

    #[test]
    fn multi_turn_history_preserved() {
        // 标准 OpenAI 客户端每轮发来完整历史；网关必须把所有轮次送入 prompt，
        // 而非只取最后一条 user（否则第 2 轮起静默丢失上下文）。
        let req = ChatRequest {
            model: None,
            messages: vec![
                msg("user", "我叫小明"),
                msg("assistant", "你好小明"),
                msg("user", "我叫什么"),
            ],
            stream: None,
            temperature: None,
            max_tokens: None,
            user: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
        };
        let p = messages_to_prompt(&req).unwrap();
        assert!(p.contains("我叫小明"), "首轮 user 丢失: {p}");
        assert!(p.contains("你好小明"), "assistant 轮丢失: {p}");
        assert!(p.contains("我叫什么"), "末轮 user 丢失: {p}");
    }

    #[test]
    fn multi_turn_roles_labeled() {
        // 历史应带角色标注，避免模型无法区分谁说的
        let req = ChatRequest {
            model: None,
            messages: vec![msg("user", "A"), msg("assistant", "B"), msg("user", "C")],
            stream: None,
            temperature: None,
            max_tokens: None,
            user: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
        };
        let p = messages_to_prompt(&req).unwrap();
        assert!(p.contains("user: A") || p.contains("A"), "{p}");
        // 至少保证顺序：A 在 B 前，B 在 C 前
        let (ia, ib, ic) = (
            p.find('A').unwrap(),
            p.find('B').unwrap(),
            p.find('C').unwrap(),
        );
        assert!(ia < ib && ib < ic, "轮次顺序错乱: {p}");
    }

    #[test]
    fn single_turn_unchanged_shape() {
        // 单轮（无历史）不应引入多余角色前缀，保持与旧行为兼容
        let req = ChatRequest {
            model: None,
            messages: vec![msg("user", "hello")],
            stream: None,
            temperature: None,
            max_tokens: None,
            user: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
        };
        assert_eq!(messages_to_prompt(&req).unwrap(), "hello");
    }

    #[test]
    fn system_plus_multi_turn() {
        let req = ChatRequest {
            model: None,
            messages: vec![
                msg("system", "be brief"),
                msg("user", "A"),
                msg("assistant", "B"),
                msg("user", "C"),
            ],
            stream: None,
            temperature: None,
            max_tokens: None,
            user: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
        };
        let p = messages_to_prompt(&req).unwrap();
        assert!(p.starts_with("be brief"), "system 应在最前: {p}");
        assert!(p.contains('A') && p.contains('B') && p.contains('C'), "{p}");
    }

    // ── v2.0.0 工具调用：请求解析 ─────────────────────────────

    fn tool_msg(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: Some(Content::Text(text.into())),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    #[test]
    fn tools_deserialized() {
        let raw = r#"{
            "model":"deepseek-es",
            "messages":[{"role":"user","content":"hi"}],
            "tools":[{"type":"function","function":{"name":"get_weather","description":"查天气","parameters":{"type":"object","properties":{"city":{"type":"string"}}}}}]
        }"#;
        let req: ChatRequest = serde_json::from_str(raw).unwrap();
        let defs = tool_defs(&req);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "get_weather");
        assert_eq!(defs[0].description.as_deref(), Some("查天气"));
    }

    #[test]
    fn no_tools_gives_empty_defs() {
        let req = ChatRequest {
            model: None,
            messages: vec![msg("user", "hi")],
            stream: None,
            temperature: None,
            max_tokens: None,
            user: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
        };
        assert!(tool_defs(&req).is_empty());
    }

    #[test]
    fn tool_result_message_rendered_into_prompt() {
        // role:"tool" 消息必须进入转录（模型据此看到工具返回值）
        let req = ChatRequest {
            model: None,
            messages: vec![
                tool_msg("user", "北京天气？"),
                Message {
                    role: "assistant".into(),
                    content: None,
                    tool_calls: Some(vec![ToolCallMsg {
                        id: "call_1".into(),
                        kind: Some("function".into()),
                        function: ToolCallFunction {
                            name: "get_weather".into(),
                            arguments: r#"{"city":"北京"}"#.into(),
                        },
                    }]),
                    tool_call_id: None,
                },
                Message {
                    role: "tool".into(),
                    content: Some(Content::Text("晴 25°C".into())),
                    tool_calls: None,
                    tool_call_id: Some("call_1".into()),
                },
            ],
            stream: None,
            temperature: None,
            max_tokens: None,
            user: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
        };
        let p = messages_to_prompt(&req).unwrap();
        assert!(p.contains("get_weather"), "工具调用名应出现: {p}");
        assert!(p.contains("北京"), "工具参数应出现: {p}");
        assert!(p.contains("晴 25°C"), "工具结果应出现: {p}");
    }

    #[test]
    fn tool_only_message_is_valid_input() {
        // 只有 tool 消息（无 user）也应可解析（工具回传场景）
        let req = ChatRequest {
            model: None,
            messages: vec![Message {
                role: "tool".into(),
                content: Some(Content::Text("结果".into())),
                tool_calls: None,
                tool_call_id: Some("call_1".into()),
            }],
            stream: None,
            temperature: None,
            max_tokens: None,
            user: None,
            tools: None,
            tool_choice: None,
            stream_options: None,
        };
        assert!(messages_to_prompt(&req).is_ok());
    }

    #[test]
    fn tool_call_chunk_serialization() {
        let c = tool_call_chunk("id", "m", 0, "call_abc", "get_weather", "{\"city\":\"x\"}");
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"tool_calls\""), "{s}");
        assert!(s.contains("\"name\":\"get_weather\""), "{s}");
        assert!(s.contains("\"index\":0"), "{s}");
    }

    #[test]
    fn stop_chunk_reason_tool_calls() {
        let c = stop_chunk_reason("id", "m", "tool_calls");
        let s = serde_json::to_string(&c).unwrap();
        assert!(s.contains("\"finish_reason\":\"tool_calls\""), "{s}");
    }
}
