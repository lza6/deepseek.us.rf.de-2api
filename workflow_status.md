# workflow_status.md — 终局闭环总审计

> 本项目为 **Rust 单二进制 API 网关**（`deepseek.es` → OpenAI/Anthropic 兼容）。
> **无前端/UI**（纯 HTTP API）——「UI/按钮/前端衔接」类检查**不适用**，已如实标注。

## 任务契约（本轮）

| # | 子任务 | 输入 | 输出 | 验收标准 | 状态 |
|---|--------|------|------|---------|------|
| T0 | 核实 H3/H4/M1-M12 | 上下文清单 | 逐项代码证据 | 每项有 file:line + 测试 | ✅ 已核实（见下） |
| T1 | 独立契约审查 | 源码 | 问题清单 | 每个含 file:line + 复现 | ⚠️ 代理超时未返回；**我自行完成契约边界测试并发现真 bug**（见 T5） |
| T2 | 独立并发/资源审查 | 源码 | 问题清单 | 同上 | ⚠️ 代理超时未返回 |
| T3 | 独立安全审查 | 源码 | 问题清单 | 同上 | ⚠️ 代理超时未返回 |
| T4 | 文档一致性审查 | README/docs vs 代码 | 问题清单 | 声称 vs 实际 | ⚠️ 代理超时未返回 |
| T5 | 修复发现的问题 | 问题清单 | 代码改动 | 测试 + E2E | ✅ 契约缺陷已修复（422 纯文本 → 协议原生错误） |
| T6 | HTML 报告 + 测验 | 全部变更 | report.html | 可直接打开、测验可判分 | ✅ `docs/CHANGE-REPORT.html`（5 题） |
| T7 | 项目工作流 skills | 项目结构 | .claude/skills/ | 可复用 | ✅ `.claude/skills/add-endpoint/` |
| T8 | SOP / ADR / 门面文档 | 项目 | docs/ | 新人可上手 | ✅ `docs/SOP-AND-ADR.md` + `VERIFICATION_LOG.md` |
| T9 | 契约防坑 + 极限压测 | 网关 | 报告 | 真实验证 | ✅ 契约测试 4 项；压测 8×24=100% |

## T0 · H3/H4/M1-M12 实证核实（已完成）

| 项 | 代码证据 | 测试 | 真实 E2E |
|----|---------|------|---------|
| H3 Anthropic 错误体 | `errors.rs:83` `into_anthropic_response()` | `anthropic_error_body_is_anthropic_shaped` | e2e-v1 8/8 |
| H4 伪工具注入 | `api.rs:212` `inject_prompt_prefixes` + `config.rs` `pseudo_tools_enabled` | `tool_instruction_injected_when_enabled` | e2e-v1 模型实际调工具 |
| M1 CRLF 分帧 | `upstream.rs:891` `find_block_sep` | `sse_crlf_block_separator` | - |
| M2 流式客户端 | `upstream.rs:78` `read_timeout` / `:88` `http_stream` | `m2_stream_client_has_no_total_timeout` | e2e 长流 |
| M3 哨兵结构化 | `openai.rs:410` `Translated::TsRequired` | `translate_ts_required` | - |
| M4 流式 usage | `api.rs:843/966` | `anthropic_stream_usage_is_nonzero` | e2e-v1 |
| M5 nonce 重试 | `upstream.rs:701` `cache_message_once` | `m5_nonce_failure_detection` | - |
| M7 断开补结束 | `api.rs:286` `oai_finished` | `*_abrupt_close_has_*` | - |
| M8 后台预取 | `upstream.rs:430` `spawn_prefetch` | `m8_needs_prefetch_*` | e2e m8 日志 |
| M9 半开单探测 | `upstream.rs:60` `half_open`/`probe_in_flight` | `breaker_half_open_*` | - |
| M10 流式配额 429 | `api.rs:718` | `streaming_quota_exhausted_returns_429` | - |
| M11 Anthropic 上限 | `api.rs:1085` | `anthropic_nonstream_respects_max_response_bytes` | - |
| M12 未知事件日志 | `openai.rs:460` | `translate_unknown_event_ignored_m12` | - |

## 不适用项（如实标注，非"未做"）
- **UI/前端/按钮/页面**：本项目是纯 API 网关，无前端。
- **图片/视频生成、付费 API**：预算 0，且上游未启用，不做真实调用。
- **Load Balancer / Redis / Kafka / Sharding**：单二进制网关，上游是单体站点；
  已具备 rate limiting / circuit breaker / health check / multi-solver LB；其余为过度设计。
