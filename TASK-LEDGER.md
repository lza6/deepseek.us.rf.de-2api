# TASK-LEDGER.md — 完整任务清单（防遗忘）

> **用途**：记录《计划书/下一步改进指南.md》的全部任务及当前状态。
> **AI 使用方式**：每轮工作前读本文件 → 找到「待办」项 → 完成 → 更新状态。
> **最后更新**：2026-10-07（v0.10.0 后）

---

## 一、§2.1 HIGH 级 —— 全部 ✅ 已完成

| 编号 | 任务 | 状态 | 版本 | 证据 |
|------|------|------|------|------|
| H1 | SSE 多字节字符损坏 | ✅ | v0.4.0 | `upstream.rs` 字节级缓冲；`sse_multibyte_*` 测试 |
| H2 | 多轮历史丢弃 | ✅ | v0.4.0 | `messages_to_prompt` 渲染完整转录；`multi_turn_*` |
| H3 | Anthropic 错误体是 OpenAI 结构 | ✅ | v0.5.0 | `errors.rs::into_anthropic_response` |
| H4 | 伪工具说明未注入 | ✅ | v0.5.0 | `inject_prompt_prefixes` + `pseudo_tools_enabled` |

## 二、§2.2 MEDIUM 级

| 编号 | 任务 | 状态 | 版本 | 证据 |
|------|------|------|------|------|
| M1 | CRLF 流级分帧 | ✅ | v0.4.0 | `find_block_sep`；`sse_crlf_block_separator` |
| M2 | 长流被总超时截断 | ✅ | v0.6.0 | 双客户端 + `read_timeout` |
| M3 | `__TS_REQUIRED__` 哨兵泄漏 | ✅ | v0.5.0 | `Translated::TsRequired` |
| M4 | Anthropic 流式 usage 恒 0 | ✅ | v0.5.0 | `prompt_tokens_c` / 输出估算 |
| M5 | nonce 失效不重试 | ✅ | v0.6.0 | `cache_message_once` + 重试 |
| M6 | finish_reason 单一取值 | ⛔ 不适用 | — | 上游 `done` 只带 `{finished:true}`，**不伪造** |
| M7 | 断开时结束信号不完整 | ✅ | v0.5.0 | `oai_finished` / `finished` 补发 |
| M8 | auth_lock 头阻塞 | ✅ | v0.6.0 | `spawn_prefetch`；真实日志验证 |
| M9 | 熔断半开全放行 | ✅ | v0.6.0 | `half_open` + `probe_in_flight` |
| M10 | 流式配额非 429 | ✅ | v0.8.0 | `start_stream` 首事件探测 |
| M11 | Anthropic 非流式无上限 | ✅ | v0.5.0 | `api.rs` 对齐 OpenAI |
| M12 | 未知事件静默丢弃 | ✅ | v0.5.0 | `tracing::debug!` |

## 三、§2.3 LOW 级

| 编号 | 任务 | 状态 | 证据 |
|------|------|------|------|
| L1 | session_id/conversation_uuid 合并 | ✅ | v0.8.0 `session_id_of()` 支持独立 session_id |
| L2 | `config()`/`fetch_page_config`/`parse_page_config` 死代码 | ✅ | v0.8.0 已删（grep 确认 0 命中） |
| L3 | `ChatChunk` 无 usage；不支持 `stream_options.include_usage` | ✅ v0.11.0 | usage_chunk + stream_options |
| L4 | Anthropic 消息 id 复用 `chatcmpl-` | ✅ v0.11.0 | `to_anthropic_msg_id` |
| L5 | Anthropic 流首帧即错误仍先发 message_start | ⏳ **待办** | 事件顺序 |
| L6 | `now_secs()` 每 chunk 重取，`created` 不一致 | ⏳ **待办** | 流内应固定 created |
| L7 | `message_delta.usage` 含 `input_tokens` | ✅ v0.11.0 | `OutputUsage` |
| L8 | Anthropic 端点不走 ResponseCache | ✅ v0.11.0 | 已接入 |
| L9 | `deterministic_uuid` 用 FNV-1a | ℹ️ 已知 | 影响面小；可改加密哈希 |
| L10 | 错误体 `code` 与 `type` 恒相同 | ⏳ **待办** | OpenAI 中 code 常更细 |
| L11 | `SolverPool::is_empty`/`SessionStore::is_empty` 零调用 | ✅ 保留 | clippy `len_without_is_empty` 要求成对，非垃圾 |
| L12 | dev-dep `http-body-util` 未用 | ✅ | v0.8.0 已删（grep 确认 0） |

## 四、§2.4 残留物 —— ✅ 已完成

| 项 | 状态 |
|----|------|
| `源代码/`（第三方 JS）移出追踪 | ✅ v0.8.0（`git rm --cached`，本地保留） |
| `抓包验证/`（探针）移出追踪 | ✅ v0.8.0 |
| `分析文档/` 保留 | ✅（被 `models.rs` 引用） |
| `计划书/` 旧指南覆盖 | ✅ 已提交 |

## 五、§3 上游协议覆盖缺口

| 能力 | 状态 | 结论 |
|------|------|------|
| 会话管理 API | ✅ v0.8.0 | `/v1/conversations` 真实 E2E |
| 余额查询 | ✅ v0.8.0 | `/v1/balance` 真实 E2E |
| `image_inputs` | ⛔ 不适用 | 上游 `allowImages=false` |
| 向量库 RAG | ⛔ 不适用 | 上游未接入 |
| `previous_openai_response_id` | ⛔ 不适用 | provider=DeepSeek |
| 联网/grounding | ⛔ 不适用 | 旗标全 false |
| `user_client_message_id` | ⛔ 不适用 | 无可证实效果，转发=伪造 |
| `post_id` | ⛔ 不适用 | 同上 |
| `openai_response_id` SSE 事件 | ⛔ 不适用 | 本站不产生 |
| status/citations/grounding/display_form/warning 事件 | ✅ 部分 | M12 已加日志；本站不产生，无法真实透传 |
| `dsbx_get_bot_model` | ⛔ 不适用 | 上游不暴露模型 id |

## 六、§4.2 v1.5.0 路线图

| 序号 | 任务 | 状态 |
|------|------|------|
| 2.1 | 半开熔断单探测 | ✅ v0.6.0 |
| 2.2 | nonce 重试 | ✅ v0.6.0 |
| 2.3 | 后台预取 | ✅ v0.6.0 |
| 2.4 | Anthropic 接入缓存 | ✅ v0.11.0（= L8） |
| 2.5 | 配额预检/展示 | ✅ v0.8.0（`/v1/balance`） |
| 2.6 | 上游事件全量透传 | ⛔ 不适用（本站不产生） |
| 2.7 | session_id/conversation_uuid 分层 | ✅ v0.8.0（= L1） |
| 2.8 | `msg_` 前缀 + finish_reason 映射 | ✅ v0.11.0（L4 完成；M6 不适用） |
| 2.9 | 流式 usage 支持 `stream_options.include_usage` | ✅ v0.11.0（= L3） |

## 七、§6 架构扩展建议

| 序号 | 任务 | 状态 |
|------|------|------|
| 6.1 | 认证层后台预取 | ✅ v0.6.0 |
| 6.2 | 协议族抽象（trait Protocol） | ⏳ 待办（结构性重构，低优先） |
| 6.3 | SSE 字节级状态机 | ✅ v0.4.0（H1 已实现） |
| 6.4 | 可观测性指标 | ⏳ 部分（`/admin/api/status` 已有基础统计） |
| 6.5 | 缓存语义化 | ⏳ 部分（cacheable 已有；temperature 未纳入） |
| 6.6 | 安全加固（admin 限流/token 哈希） | ⏳ 待办 |

---

## ⏳ 真正待办清单（下一步执行）

按优先级：

1. **L3 / 2.9 流式 usage + `stream_options.include_usage`** —— 真实客户端会请求，缺则 token 统计坏。
2. **L4 / 2.8 Anthropic 消息 id 用 `msg_` 前缀** —— 部分客户端据此判别类型。
3. **L7 `message_delta.usage` 去掉 `input_tokens`** —— 规范只含 output_tokens。
4. **L6 流内 `created` 固定** —— 同一流内不一致。
5. **L5 Anthropic 流首帧即错误的顺序**。
6. **L8 / 2.4 Anthropic 接入响应缓存**。
7. **L10 错误体 `code` 与 `type` 区分**。
8. **6.4 可观测性指标**（`/admin/api/status` 扩展）。
9. **6.6 安全加固**（admin 限流、token 哈希）。
10. **6.2 协议族抽象**（结构性，最后做）。

## ✅ 已确认「不适用」（勿浪费时间）

M6、图片、向量库 RAG、OpenAI 响应链、联网接地、`user_client_message_id`、`post_id`、
`dsbx_get_bot_model`、L11（保留）、UI/前端。
