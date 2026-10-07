//! 增强特性：提示词语言注入（P3-6）与伪工具调用（P3-7）。
//!
//! 二者均在**网关侧**对 prompt / 输出做纯文本变换，不改上游协议。

/// H4：组合注入——把语言指令与（可选的）伪工具说明一并前置到 prompt。
///
/// 修复前：`TOOL_INSTRUCTION` 虽已定义，但**从未注入**，模型不知道可以发工具块，
/// 导致伪工具功能实际不可用。此函数确保启用时说明真正进入 prompt。
/// 两段都为空时返回原 prompt（无副作用）。
///
/// 取代旧的 `inject_system_prompt`（仅支持语言、无法注入工具说明，已删除）。
pub fn inject_prompt_prefixes(prompt: &str, language_suffix: &str, pseudo_tools: bool) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let lang = language_suffix.trim();
    if !lang.is_empty() {
        parts.push(lang);
    }
    if pseudo_tools {
        parts.push(TOOL_INSTRUCTION);
    }
    if parts.is_empty() {
        return prompt.to_string();
    }
    format!("{}\n\n{prompt}", parts.join("\n\n"))
}

/// P3-7：伪工具调用——从模型输出中提取 fenced `tool` 代码块内的 JSON 调用。
///
/// 约定格式（模型被指示按此输出，fence 语言标记为 tool / json:tool / json-tool）：
/// 围栏内容为一行 JSON：`{"name":"...","arguments":{...}}`。
///
/// 返回解析成功的调用列表；无则空 vec。同时返回去除工具块后的可见文本。
/// 非法 JSON 忽略，不报错。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// 解析结果。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolParse {
    /// 可见文本（已移除工具块）
    pub text: String,
    /// 解析出的调用
    pub calls: Vec<ToolCall>,
}

/// 从完整输出中解析伪工具调用。
///
/// 只识别 ` ```tool ` / ` ```json:tool ` 开头的围栏块；非法 JSON 忽略（不报错）。
pub fn parse_tool_calls(output: &str) -> ToolParse {
    let mut calls = Vec::new();
    let mut text = String::new();
    let mut rest = output;

    while let Some(start) = find_fence(rest) {
        let (before, after_open) = rest.split_at(start);
        text.push_str(before);
        // after_open 形如 "```tool\n.....\n```..." 或 "```json:tool\n..."
        let Some(nl) = after_open.find('\n') else {
            // 未闭合：原样保留
            text.push_str(after_open);
            rest = "";
            break;
        };
        let lang = after_open[..nl].trim_start_matches('`').trim();
        let body_and_rest = &after_open[nl + 1..];
        let is_tool = matches!(lang, "tool" | "json:tool" | "json-tool");
        if !is_tool {
            // 非工具块：原样保留围栏与内容
            text.push_str(&after_open[..nl]);
            text.push('\n');
            rest = body_and_rest;
            continue;
        }
        match body_and_rest.find("\n```") {
            Some(end) => {
                let body = &body_and_rest[..end];
                rest = &body_and_rest[end + 4..];
                for line in body.lines() {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(call) = serde_json::from_str::<ToolCall>(line) {
                        calls.push(call);
                    } else if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                        // 容错：{"name":..,"parameters":..} 形式
                        if let Some(name) = v.get("name").and_then(|n| n.as_str()) {
                            let args = v
                                .get("arguments")
                                .or_else(|| v.get("parameters"))
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);
                            calls.push(ToolCall {
                                name: name.to_string(),
                                arguments: args,
                            });
                        }
                    }
                }
            }
            None => {
                // 未闭合：保留剩余
                text.push_str(after_open);
                rest = "";
                break;
            }
        }
    }
    text.push_str(rest);
    ToolParse {
        text: text.trim().to_string(),
        calls,
    }
}

/// 找到下一个工具围栏起点（```tool 或 ```json:tool）。
fn find_fence(s: &str) -> Option<usize> {
    let a = s.find("```tool");
    let b = s.find("```json:tool");
    let c = s.find("```json-tool");
    [a, b, c].into_iter().flatten().min()
}

/// 用于注入到 prompt 的工具使用说明（当启用伪工具时）。
pub const TOOL_INSTRUCTION: &str = "\
You may call tools by emitting a fenced block exactly like:\n\
```tool\n\
{\"name\":\"<tool_name>\",\"arguments\":{...}}\n\
```\n\
Available tools: get_time (arguments: {}), echo (arguments: {\"text\":\"...\"}).\n\
Only emit tool blocks when a tool is actually needed.";

/// 内置示例工具执行（本地函数，无外部依赖）。
pub fn execute_tool(call: &ToolCall) -> serde_json::Value {
    match call.name.as_str() {
        "get_time" => serde_json::json!({
            "ok": true,
            "result": chrono::Utc::now().to_rfc3339(),
        }),
        "echo" => {
            let text = call
                .arguments
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("");
            serde_json::json!({ "ok": true, "result": text })
        }
        other => serde_json::json!({ "ok": false, "error": format!("未知工具: {other}") }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_prepended() {
        assert_eq!(
            inject_prompt_prefixes("hi", "Responde en chino", false),
            "Responde en chino\n\nhi"
        );
    }

    // ── H4 回归：伪工具说明必须在启用时注入 ─────────────────────

    #[test]
    fn tool_instruction_injected_when_enabled() {
        let p = inject_prompt_prefixes("hi", "", true);
        assert!(p.contains("get_time"), "工具说明未注入: {p}");
        assert!(p.contains("```tool"), "工具块格式未注入: {p}");
        assert!(p.ends_with("hi"), "原 prompt 应保留在末尾: {p}");
    }

    #[test]
    fn tool_instruction_absent_when_disabled() {
        let p = inject_prompt_prefixes("hi", "", false);
        assert_eq!(p, "hi");
        assert!(!p.contains("get_time"));
    }

    #[test]
    fn language_and_tools_both_injected() {
        let p = inject_prompt_prefixes("hi", "用中文回答", true);
        assert!(p.contains("用中文回答"), "{p}");
        assert!(p.contains("get_time"), "{p}");
        assert!(p.ends_with("hi"), "{p}");
    }

    #[test]
    fn no_prefixes_is_noop() {
        assert_eq!(inject_prompt_prefixes("hi", "", false), "hi");
        assert_eq!(inject_prompt_prefixes("hi", "   ", false), "hi");
    }

    #[test]
    fn parse_no_tool() {
        let p = parse_tool_calls("just text");
        assert!(p.calls.is_empty());
        assert_eq!(p.text, "just text");
    }

    #[test]
    fn parse_single_tool() {
        let out = "Let me check.\n```tool\n{\"name\":\"get_time\",\"arguments\":{}}\n```";
        let p = parse_tool_calls(out);
        assert_eq!(p.calls.len(), 1);
        assert_eq!(p.calls[0].name, "get_time");
        assert_eq!(p.text, "Let me check.");
    }

    #[test]
    fn parse_multiple_tools_and_keep_text() {
        let out = "A\n```tool\n{\"name\":\"echo\",\"arguments\":{\"text\":\"x\"}}\n```\nB\n```json:tool\n{\"name\":\"get_time\",\"parameters\":{}}\n```\nC";
        let p = parse_tool_calls(out);
        assert_eq!(p.calls.len(), 2);
        assert_eq!(p.calls[0].name, "echo");
        assert_eq!(p.calls[1].name, "get_time");
        assert!(p.text.contains('A') && p.text.contains('B') && p.text.contains('C'));
    }

    #[test]
    fn malformed_tool_ignored() {
        let out = "```tool\n{not json}\n```\nend";
        let p = parse_tool_calls(out);
        assert!(p.calls.is_empty());
        assert_eq!(p.text, "end");
    }

    #[test]
    fn non_tool_fence_preserved() {
        let out = "code:\n```rust\nfn main(){}\n```\nend";
        let p = parse_tool_calls(out);
        assert!(p.calls.is_empty());
        assert!(
            p.text.contains("fn main(){}"),
            "普通代码块不应被吞: {}",
            p.text
        );
    }

    #[test]
    fn execute_builtin_tools() {
        let t = execute_tool(&ToolCall {
            name: "echo".into(),
            arguments: serde_json::json!({"text": "hi"}),
        });
        assert_eq!(t["result"], "hi");
        let t2 = execute_tool(&ToolCall {
            name: "get_time".into(),
            arguments: serde_json::Value::Null,
        });
        assert_eq!(t2["ok"], true);
        let t3 = execute_tool(&ToolCall {
            name: "nope".into(),
            arguments: serde_json::Value::Null,
        });
        assert_eq!(t3["ok"], false);
    }
}
