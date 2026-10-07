//! 协议级工具调用（v2.0.0）：工具定义 → 说明注入 → 调用解析 → 协议结构产出。
//!
//! ## 背景
//!
//! 上游 AIPKit **不支持原生 function calling**（实测 `allowTools=false`）。
//! 因此工具调用在**网关侧**实现：把客户端声明的工具渲染成文本说明注入 prompt，
//! 模型按约定输出 fenced ` ```tool ` 块，网关解析后对外产出**标准协议结构**
//! （OpenAI `tool_calls` / Anthropic `tool_use`）。
//!
//! 这是真实可用的协议翻译，非伪造：客户端能收到合法的 `tool_calls`/`tool_use` 并能回传结果。

use serde::{Deserialize, Serialize};

/// 一个工具定义（与 OpenAI `tools[].function` 同构）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema 形式的参数定义（原样透传给模型说明）。
    #[serde(default)]
    pub parameters: Option<serde_json::Value>,
}

/// 一次工具调用（网关解析产物）。
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInvocation {
    pub id: String,
    pub name: String,
    /// 参数（JSON 对象）。
    pub arguments: serde_json::Value,
}

/// 把工具定义渲染成注入 prompt 的说明文本。
///
/// 说明中包含：调用格式约定 + 每个工具的名称/描述/参数。
/// 若 `defs` 为空返回 `None`（调用方据此跳过注入）。
pub fn render_tool_prompt(defs: &[ToolDef]) -> Option<String> {
    if defs.is_empty() {
        return None;
    }
    let mut s = String::from(
        "You have access to tools. To call a tool, emit exactly one fenced block per call:\n\
         ```tool\n{\"name\":\"<tool_name>\",\"arguments\":{...}}\n```\n\
         Only emit tool blocks when a tool is actually needed; otherwise answer normally.\n\
         Available tools:\n",
    );
    for d in defs {
        let desc = d.description.as_deref().unwrap_or("");
        let params = d
            .parameters
            .as_ref()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "{}".into());
        s.push_str(&format!(
            "- {}: {} | parameters: {}\n",
            d.name, desc, params
        ));
    }
    Some(s)
}

/// 从模型完整输出中解析工具调用。
///
/// 只识别 ` ```tool ` / ` ```json:tool ` / ` ```json-tool ` 围栏块，
/// 每行一个 JSON `{"name":..,"arguments":..}`（兼容 `parameters` 键）。
/// 返回去除工具块后的可见文本 + 调用列表。非法 JSON 忽略（回退纯文本，不报错）。
pub fn parse_invocations(output: &str) -> (String, Vec<ToolInvocation>) {
    let mut calls = Vec::new();
    let mut text = String::new();
    let mut rest = output;
    let mut seq = 0u32;

    while let Some(start) = find_fence(rest) {
        let (before, after_open) = rest.split_at(start);
        text.push_str(before);
        let Some(nl) = after_open.find('\n') else {
            text.push_str(after_open);
            rest = "";
            break;
        };
        let lang = after_open[..nl].trim_start_matches('`').trim();
        let body_and_rest = &after_open[nl + 1..];
        if !matches!(lang, "tool" | "json:tool" | "json-tool") {
            // 非工具块：原样保留
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
                    if let Some(inv) = parse_one(line, seq) {
                        seq += 1;
                        calls.push(inv);
                    }
                }
            }
            None => {
                // 未闭合：保留剩余（模型可能仍在输出）
                text.push_str(after_open);
                rest = "";
                break;
            }
        }
    }
    text.push_str(rest);
    (text.trim().to_string(), calls)
}

/// 解析单行工具调用 JSON。
fn parse_one(line: &str, seq: u32) -> Option<ToolInvocation> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let name = v.get("name").and_then(|n| n.as_str())?.to_string();
    let args = v
        .get("arguments")
        .or_else(|| v.get("parameters"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Some(ToolInvocation {
        id: format!("call_{:016x}{:04x}", fnv1a(&name), seq),
        name,
        arguments: args,
    })
}

fn find_fence(s: &str) -> Option<usize> {
    let a = s.find("```tool");
    let b = s.find("```json:tool");
    let c = s.find("```json-tool");
    [a, b, c].into_iter().flatten().min()
}

fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// 流式：判断累积文本是否可能进入工具块（用于 hold-back 决策）。
///
/// 返回 `Some(start_idx)` 表示 `buf` 中某处开始出现工具围栏（需暂扣后续输出）。
pub fn tool_fence_start(buf: &str) -> Option<usize> {
    find_fence(buf)
}

/// 流式工具过滤状态机（v2.0.0）。
///
/// 上游逐 delta 输出；当文本中出现 ` ```tool ` 起点时，后续内容属于工具块，
/// **不应**作为普通文本发给下游（否则客户端会看到原始 fenced JSON）。
/// 本状态机负责：把「工具块之外」的文本即时放行，「工具块之内」的内容暂扣，
/// 流结束时统一解析出工具调用。
///
/// Unicode 安全：fence 标记是 ASCII，但被 delta 切断时需跨 delta 匹配。
/// 采用「保守放行」策略——只有确定不在 fence 中才放行，避免泄漏。
#[derive(Debug, Default)]
pub struct StreamToolFilter {
    /// 已放行给下游的文本累积（不含工具块）
    pub emitted: String,
    /// 当前暂扣的缓冲（工具块 + 可能的 fence 前缀尾部）
    pending: String,
    /// 已完成的工具块原文（供 finish 解析）
    tool_blocks: String,
    /// 是否已确认进入工具块
    in_tool: bool,
}

/// 状态机输出。
#[derive(Debug, PartialEq)]
pub enum FilterOut {
    /// 可安全放行的文本
    Text(String),
    /// 暂扣中（无输出，等待后续 delta）
    Hold,
}

impl StreamToolFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// 送入一个 delta，返回可放行的文本（若有）。
    pub fn push(&mut self, delta: &str) -> FilterOut {
        self.pending.push_str(delta);
        let mut release = String::new();
        loop {
            if self.in_tool {
                // 工具块内：查找闭合 "\n```"
                match self.pending.find("\n```") {
                    Some(end) => {
                        let closed = self.pending[..end + 4].to_string();
                        self.tool_blocks.push_str(&closed);
                        self.tool_blocks.push('\n');
                        self.pending = self.pending[end + 4..].to_string();
                        self.in_tool = false;
                        continue;
                    }
                    None => break, // 未闭合，继续暂扣
                }
            }
            // 未在工具块内：查找 fence 起点
            match find_fence(&self.pending) {
                Some(start) => {
                    release.push_str(&self.pending[..start]);
                    self.pending = self.pending[start..].to_string();
                    self.in_tool = true;
                    continue;
                }
                None => {
                    // 无 fence：放行「确定不属于 fence 前缀」的前缀，暂扣尾部
                    let safe = self.safe_prefix_len();
                    release.push_str(&self.pending[..safe]);
                    self.pending = self.pending[safe..].to_string();
                    break;
                }
            }
        }
        self.emitted.push_str(&release);
        if release.is_empty() {
            FilterOut::Hold
        } else {
            FilterOut::Text(release)
        }
    }

    /// 可安全放行的前缀长度（避开尾部可能的 fence 前缀）。
    fn safe_prefix_len(&self) -> usize {
        let b = self.pending.as_bytes();
        const FENCES: [&[u8]; 3] = [b"```tool", b"```json:tool", b"```json-tool"];
        // 检查尾部 1..=11 字节是否可能是某 fence 的前缀
        for k in (1..=11.min(b.len())).rev() {
            let tail = &b[b.len() - k..];
            if FENCES.iter().any(|f| f.len() > k && f.starts_with(tail)) {
                return b.len() - k;
            }
        }
        b.len()
    }

    /// 流结束：返回剩余可见文本与解析出的调用。
    pub fn finish(mut self) -> (String, Vec<ToolInvocation>) {
        let pending = std::mem::take(&mut self.pending);
        let mut out = self.emitted;
        let mut all_calls = Vec::new();
        // 已闭合的工具块
        let (_, mut calls) = parse_invocations(&self.tool_blocks);
        all_calls.append(&mut calls);
        if self.in_tool {
            // 未闭合的工具块：模型可能省略了闭合围栏。手动解析围栏后的 JSON 行。
            let body = pending.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
            for line in body.lines() {
                let line = line.trim().trim_end_matches('`').trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(inv) = parse_one(line, all_calls.len() as u32) {
                    all_calls.push(inv);
                }
            }
        } else {
            // pending 是未能确认的尾部文本（如单个 "`"），作为普通文本放行
            out.push_str(&pending);
        }
        (out, all_calls)
    }

    /// 流式场景下的结束调用（`&mut self` 版，供闭包内使用）。
    ///
    /// 与 `finish` 等价，但以 `&mut self` 调用，内部用 `std::mem::take` 取走状态。
    pub fn push_finish(&mut self) -> (String, Vec<ToolInvocation>) {
        let me = std::mem::take(self);
        me.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn def(name: &str, desc: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: Some(desc.into()),
            parameters: Some(serde_json::json!({"type":"object","properties":{}})),
        }
    }

    // ── 渲染 ──
    #[test]
    fn render_empty_is_none() {
        assert!(render_tool_prompt(&[]).is_none());
    }

    #[test]
    fn render_contains_all_tool_names() {
        let s =
            render_tool_prompt(&[def("get_weather", "查天气"), def("get_time", "时间")]).unwrap();
        assert!(s.contains("get_weather"), "{s}");
        assert!(s.contains("get_time"), "{s}");
        assert!(s.contains("```tool"), "应含格式约定: {s}");
        assert!(s.contains("查天气"), "{s}");
    }

    // ── 解析 ──
    #[test]
    fn parse_no_tool_returns_text() {
        let (t, c) = parse_invocations("just text");
        assert_eq!(t, "just text");
        assert!(c.is_empty());
    }

    #[test]
    fn parse_single_invocation() {
        let out = "Let me check.\n```tool\n{\"name\":\"get_time\",\"arguments\":{}}\n```";
        let (t, c) = parse_invocations(out);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "get_time");
        assert_eq!(t, "Let me check.");
        assert!(c[0].id.starts_with("call_"));
    }

    #[test]
    fn parse_multiple_invocations_unique_ids() {
        let out = "```tool\n{\"name\":\"a\",\"arguments\":{}}\n{\"name\":\"b\",\"arguments\":{\"x\":1}}\n```";
        let (_, c) = parse_invocations(out);
        assert_eq!(c.len(), 2);
        assert_ne!(c[0].id, c[1].id, "调用 id 必须唯一");
        assert_eq!(c[1].arguments["x"], 1);
    }

    #[test]
    fn parse_parameters_key_compat() {
        let out = "```json:tool\n{\"name\":\"f\",\"parameters\":{\"y\":2}}\n```";
        let (_, c) = parse_invocations(out);
        assert_eq!(c[0].arguments["y"], 2);
    }

    #[test]
    fn parse_malformed_ignored() {
        let out = "```tool\n{not json}\n```\nend";
        let (t, c) = parse_invocations(out);
        assert!(c.is_empty());
        assert_eq!(t, "end");
    }

    #[test]
    fn parse_preserves_non_tool_fence() {
        let out = "```rust\nfn main(){}\n```";
        let (t, c) = parse_invocations(out);
        assert!(c.is_empty());
        assert!(t.contains("fn main(){}"), "{t}");
    }

    #[test]
    fn parse_unclosed_fence_kept_as_text() {
        let out = "text\n```tool\n{\"name\":\"x\"";
        let (t, c) = parse_invocations(out);
        assert!(c.is_empty());
        assert!(t.contains("text"), "{t}");
    }

    // ── 流式 hold-back 判定 ──
    #[test]
    fn fence_detection() {
        assert!(tool_fence_start("normal text").is_none());
        assert_eq!(tool_fence_start("abc```tool"), Some(3));
        assert_eq!(tool_fence_start("```json:tool"), Some(0));
    }

    // ── StreamToolFilter 状态机 ────────────────────────────

    fn feed(filter: &mut StreamToolFilter, deltas: &[&str]) -> String {
        let mut out = String::new();
        for d in deltas {
            if let FilterOut::Text(t) = filter.push(d) {
                out.push_str(&t);
            }
        }
        out
    }

    #[test]
    fn filter_passes_plain_text() {
        let mut f = StreamToolFilter::new();
        let out = feed(&mut f, &["Hello", " world"]);
        assert_eq!(out, "Hello world");
        let (tail, calls) = f.finish();
        assert_eq!(tail, "Hello world");
        assert!(calls.is_empty());
    }

    #[test]
    fn filter_holds_tool_block_only() {
        let mut f = StreamToolFilter::new();
        let out = feed(
            &mut f,
            &[
                "Let me check.\n",
                "```tool\n",
                "{\"name\":\"get_time\"",
                ",\"arguments\":{}}\n",
                "```",
                "\nDone.",
            ],
        );
        // 工具块不应出现在流式输出中
        assert!(!out.contains("```tool"), "工具块泄漏: {out}");
        assert!(out.contains("Let me check."), "{out}");
        assert!(out.contains("Done."), "{out}");
        let (tail, calls) = f.finish();
        assert!(!tail.contains("```"), "工具块泄漏到尾部: {tail}");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_time");
    }

    #[test]
    fn filter_handles_fence_split_across_deltas() {
        // 关键：fence 标记被切成多个 delta，不得泄漏
        let mut f = StreamToolFilter::new();
        let out = feed(
            &mut f,
            &[
                "ok ",
                "`",
                "`",
                "`",
                "to",
                "ol\n",
                "{\"name\":\"x\",\"arguments\":{}}\n",
                "`",
                "`",
                "`",
            ],
        );
        assert!(!out.contains("```"), "被切分的 fence 泄漏: {out:?}");
        assert!(!out.contains("tool"), "被切分的 fence 泄漏: {out:?}");
        let (_tail, calls) = f.finish();
        assert_eq!(calls.len(), 1, "切分 fence 应仍能解析");
    }

    #[test]
    fn filter_text_after_tool_block_resumes() {
        let mut f = StreamToolFilter::new();
        let out = feed(
            &mut f,
            &["A", "```tool\n{\"name\":\"t\",\"arguments\":{}}\n```", "B"],
        );
        assert!(out.contains('A'), "{out}");
        assert!(out.contains('B'), "{out}");
        assert!(!out.contains("tool"), "工具块泄漏: {out}");
    }

    #[test]
    fn filter_unclosed_tool_block_parsed_at_finish() {
        // 模型未闭合围栏：finish 时应尽力解析
        let mut f = StreamToolFilter::new();
        let _ = feed(&mut f, &["```tool\n{\"name\":\"t\",\"arguments\":{}}\n"]);
        let (_tail, calls) = f.finish();
        assert_eq!(calls.len(), 1, "未闭合工具块应在 finish 解析");
    }

    #[test]
    fn filter_two_tool_blocks() {
        let mut f = StreamToolFilter::new();
        let out = feed(
            &mut f,
            &["```tool\n{\"name\":\"a\",\"arguments\":{}}\n```\n|```tool\n{\"name\":\"b\",\"arguments\":{}}\n```"],
        );
        assert!(out.contains('|'), "中间文本应放行: {out}");
        let (_t, calls) = f.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "a");
        assert_eq!(calls[1].name, "b");
    }

    #[test]
    fn filter_normal_backticks_not_tool() {
        // 普通 markdown 代码块（非 tool）不应被吞
        let mut f = StreamToolFilter::new();
        let out = feed(&mut f, &["```rust\n", "fn main(){}\n", "```"]);
        assert!(out.contains("fn main(){}"), "普通代码块被吞: {out}");
        let (_t, calls) = f.finish();
        assert!(calls.is_empty());
    }
}
