# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/) 与 [语义化版本](https://semver.org/)。

## [0.9.0] - 2026-10-07

**契约防坑 + 交付物完善**：修复一个会让真实 SDK 解析失败的契约缺陷；新增变更报告、项目 skill、SOP/ADR、验证记录。

### Fixed
- **HIGH · 请求体反序列化失败返回 422 纯文本（契约缺陷）**：
  `/v1/messages`、`/v1/messages/count_tokens`、`/v1/chat/completions` 原用 axum 的
  `Json<T>` 提取器。当请求体缺字段（如 `messages`）或非法 JSON 时，axum 在**进入 handler 之前**
  就返回 **422 + 纯文本**（`Failed to deserialize the JSON body...`），**绕过了** H3 引入的
  `into_anthropic_response()`。真实客户端（Claude Code / OpenAI SDK / Anthropic SDK）解析该纯文本会失败。
  修复：改为接收 `axum::body::Bytes` 手动解析，失败时返回**协议原生错误结构**
  （OpenAI `{"error":{...}}` / Anthropic `{"type":"error","error":{...}}`），HTTP 400。
  新增测试：`anthropic_malformed_body_is_anthropic_shaped`、`anthropic_invalid_json_is_anthropic_shaped`、
  `openai_malformed_body_is_openai_shaped`、`extra_fields_are_tolerated`。

### Added（交付物）
- `docs/CHANGE-REPORT.html`：v0.4.0→v0.9.0 变更报告（含 5 题验证测验，可自动判分）。
- `.claude/skills/add-endpoint/SKILL.md`：新增端点/功能的完整工作流（读记忆→判断过时→契约铁则→TDD→E2E→文档同步）。
- `docs/SOP-AND-ADR.md`：运维标准作业流程 + 6 条架构决策记录（ADR-001..006）。
- `VERIFICATION_LOG.md`：验证记录与「已优化项」表（避免重复优化同一处）。
- `workflow_status.md`：本轮终局审计的任务契约与进度。

### Verified
- `cargo test --all`：**135 单测 + 46 集成全绿**（较 v0.8.0 新增 4 集成）；fmt / clippy 干净。
- 契约边界实测：畸形请求体 → 协议原生错误结构（非 422 纯文本）。

---

## [0.8.0] - 2026-10-07

**新端点 + 流式配额修复 + 仓库瘦身**（v1.5.0 收尾）：新增 `/v1/balance` 与 `/v1/conversations`；
修复 M10（流式配额未映射 429）；清理死代码与残留。

### Added
- **`GET /v1/balance`**：查询**网关身份**的上游配额/余额。
  抓首页解析 `dsgtConfig`（restUrl + nonce）→ 带 `X-WP-Nonce` GET `dsgt/v1/balance?bot_id=`。
  返回上游 `{balance, free:{limit,remaining,used}, ...}` 原样透传。
  **如实说明**：余额绑定网关的浏览器身份（dsts cookie），**非下游用户余额**，用于配额提示。
- **`GET /v1/conversations`**：列出上游会话（`aipkit_get_conversations_list`），
  响应归一化为 `{object:"list", data:[{id,title}]}`（兼容 `conversation_uuid`/`uuid`/`id` 与 `title`/`name`）。
- **`DELETE /v1/conversations?id=<uuid>`**：删除单条上游会话（`aipkit_delete_single_conversation`）。缺 id → 400。
- `src/upstream.rs`：`fetch_balance()` / `list_conversations()` / `delete_conversation()` / 通用 `ajax_call()`；
  `parse_dsgt_config()`（大括号配平提取，容忍转义与嵌套）。
- 脚本 `scripts/e2e-meta.mjs`。

### Fixed
- **MEDIUM · 流式请求的配额耗尽返回 200 + 错误帧（M10）**：`start_stream` 探测首事件时，
  若首事件为 `Quota`，此前会把它作为错误帧写进已开始的 SSE 流（HTTP 200），
  多数 SDK 不会当作限流。修复：首事件即配额/错误时，**在响应头发出前**
  返回 `AppError::QuotaExhausted`（429）/ `UpstreamStream`（502）。
  两协议（OpenAI/Anthropic）流式均受益。新增测试 `streaming_quota_exhausted_returns_429`。

### Removed（死代码与残留清理）
- 删除死代码链：`UpstreamClient::config()`、`fetch_page_config()`、`parse_page_config()`、
  `PageConfig`、`decode_entities()`（均**零生产调用点**；`bot_id` 永远取配置，从不从页面刷新）。
- `Cargo.toml`：移除未使用的 `http`（直接依赖）与 dev-deps `http-body-util` / `tower`；
  `tower-http` 特性由 `["cors","trace"]` 收窄为 `["cors"]`（无 `TraceLayer`）。
- **仓库瘦身**：`源代码/`（含上游前端第三方 JS，678K）、`抓包验证/`（一次性探针，449K）
  **移出 git 追踪**（`git rm --cached`，本地文件保留）并加入 `.gitignore`。共减约 1.1MB。
  `分析文档/` **保留**（被 `src/models.rs` 引用，有长期价值）。

### Not Applicable（如实说明，不伪造）
- 上游"能力缺口"经核实**不可落地**：图片输入（`allowImages=false`）、向量库 RAG、
  `previous_openai_response_id`（provider≠OpenAI）、联网/Google 接地（旗标全 false）。
  这些 bot 侧未启用，网关无论如何转发参数都不会改变上游行为 → **不实现**。
- `features.rs`（伪工具，网关本地执行）与 `tools.rs`（协议级工具，客户端执行）
  是**不同特性**，其 ```tool 围栏解析虽形似但语义不同 → **保留，不合并**（避免耦合两个特性的风险）。

### Verified
- `cargo test --all`：**135 单测 + 42 集成全绿**（较 v0.7.0 新增 3 单测 + 4 集成）；
  `cargo fmt --all -- --check` 与 `cargo clippy --all-targets -- -D warnings` 均干净。
- **真实 E2E**（连真实上游 deepseek.es + cf_solver）：**5/5 通过**——
  `/v1/balance` 真实返回 `{"balance":0,"free":{"limit":30000,"remaining":30000,"used":0},...}`；
  `/v1/conversations` 返回 `{"data":[],"object":"list"}`；`DELETE` 缺 id → 400。
- **压测**：8 并发 × 24 请求 = **100% 成功**。

---

## [0.7.0] - 2026-10-07


**协议级工具调用（v2.0.0 核心能力）**：OpenAI `tools`/`tool_calls` 与 Anthropic
`tools`/`tool_use`/`tool_result` 全链路支持，流式与非流式、两协议均覆盖。

> **上游约束（如实说明）**：上游 AIPKit **不支持原生 function calling**（实测 `allowTools=false`）。
> 本实现是**网关侧协议翻译**：把客户端声明的工具渲染成说明注入 prompt，模型按约定输出
> fenced ` ```tool ` 块，网关解析后产出**标准协议结构**（非伪造——客户端能收到合法的
> `tool_calls`/`tool_use` 并回传结果，真实模型能基于结果作答，已由真实 E2E 证明）。

### Added
- **新模块 `src/tools.rs`**：
  - `ToolDef` / `ToolInvocation` 中间表示。
  - `render_tool_prompt()`：把工具定义渲染为注入 prompt 的说明（含调用格式约定）。
  - `parse_invocations()`：解析模型输出中的 ```tool 块（兼容 `arguments`/`parameters` 键，多调用、id 唯一）。
  - `StreamToolFilter`：**流式 hold-back 状态机**——工具块内容不泄漏给下游文本流，
    且正确处理 **fence 标记被 delta 切断**（如 "`" + "`" + "`tool"）的边界；流结束时解析工具调用。
- **OpenAI 侧**：请求解析 `tools`/`tool_choice`；`role:"tool"` 消息与 assistant `tool_calls`
  渲染进转录；非流式产出 `choices[].message.tool_calls` + `finish_reason:"tool_calls"`；
  流式产出 `delta.tool_calls`（含 `index`/`id`/`function.name`/`function.arguments`）。
- **Anthropic 侧**：请求解析 `tools`（`input_schema`）；`tool_use`/`tool_result` 内容块；
  非流式产出 `content[].type=="tool_use"` + `stop_reason:"tool_use"`；
  流式产出 `content_block_start(tool_use)` + `input_json_delta`。
- 脚本 `scripts/e2e-tools.mjs`（真实工具往返 E2E）。
- 计划文档 `计划书/工具调用实施计划.md`。

### Fixed
- **Anthropic `tool_result` 内容丢失（真实 bug，E2E 发现）**：Anthropic 规范中 `tool_result`
  的结果放在 **`content` 字段**（可为字符串或块数组），而非 `text`。原实现只读 `text`，
  导致客户端回传的工具结果**被静默丢弃**，模型第 2 轮无法基于结果作答（真实 E2E 首次暴露：
  第 2 轮返回空串）。修复：新增 `TextBlock::readable()`（覆盖 `text` 与 `tool_result.content`），
  并修正 `render_turn` / `is_empty` 使用它。修复后真实 E2E 第 2 轮正确回答。

### Verified
- `cargo test --all`：**132 单测 + 38 集成全绿**（较 v0.6.0 新增 31 单测 + 5 集成）；
  `cargo fmt --all -- --check` 与 `cargo clippy --all-targets -- -D warnings` 均干净。
- **真实 E2E（OpenAI 工具往返，连真实上游 deepseek.es + cf_solver）**：
  ```
  第1轮 finish_reason: tool_calls
  tool_calls: [{"id":"call_...","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"北京\"}"}}]
  本地执行工具: get_weather {"city":"北京"} → {"city":"北京","temp_c":25,"condition":"晴"}
  第2轮最终回答: "根据查询结果，北京今天天气**晴**，气温约 **25°C**，体感比较舒适，适合外出。"
  ```
- **真实 E2E（Anthropic 工具往返）**：
  ```
  stop_reason: tool_use
  tool_use: {"type":"tool_use","id":"call_...","name":"get_weather","input":{"city":"上海"}}
  本地执行 → {"city":"上海","temp_c":22,"condition":"多云"}
  第2轮: "上海今天天气：多云，气温约 22°C。"
  ```
- **压测**：8 并发 × 24 请求 = **100% 成功**，p50=1976ms、p99=3305ms、QPS 4.47。

---

## [0.6.0] - 2026-10-07


**可靠性加固批次**（指南 v1.5.0 首批）：关闭 M2 / M5 / M8 / M9，并如实标注 M6 为上游能力缺口。

### Fixed
- **MEDIUM · 长回答被 120s 总超时截断（M2）**：`reqwest` 的 `.timeout()` 覆盖从建连到 body 读完的
  **全过程**，流式生成超过 `http_timeout_secs` 会触发 body 读取错误 → 下游收到 `__stream_error__`
  而非正常结束。修复：**分离两个 HTTP 客户端**——普通 AJAX 客户端保留总超时；
  新增 **SSE 流式客户端**（无总超时，仅 `connect_timeout` + `read_timeout` 空闲读超时）。
  新增配置 `connect_timeout_secs`（默认 20）。
- **MEDIUM · nonce 失效不重试原请求（M5）**：`cache_message` 检测到 nonce/security 类失败时
  只刷新 nonce 却**立即返回错误**，本次请求仍失败。修复：抽出 `cache_message_once`，
  外层在疑似 nonce 失效时刷新后**重试一次**（该操作幂等，重试安全）。
  `stream_chat` 收到 403 现映射为 `TsRequired`（可触发上层重认证重试），而非普通上游错误。
- **MEDIUM · auth_lock 头阻塞（M8）**：cookie TTL 到期后首个请求内联求解 45-65s，
  期间所有并发请求排队。修复：新增**后台预取任务**（`UpstreamClient::spawn_prefetch`，
  `main.rs` 启动时挂载）——每 30s 检查，cookie 剩余 TTL < 20% 时主动续期，
  且**不清空旧 cookie**（续期期间并发请求仍走快速路径，不被阻塞）。
- **MEDIUM · 熔断半开状态放行全部并发探测（M9）**：冷却到点后清空状态即放行，
  N 个并发请求会同时打爆 cf_solver；且半开后一次失败只从 0 计数到 1，需再次累计到阈值才重开，保护滞后。
  修复：引入 `half_open` + `probe_in_flight` 状态——半开态**只放行一个探测**（其余拒绝），
  探测失败**立即重新熔断**（不等阈值累积），请求结束释放探测标记。

### Not Applicable
- **MEDIUM · finish_reason 单一取值（M6）**：经核实上游 `done` 事件仅携带 `{"finished":true}`，
  **不提供** stop/length/tool_calls 的区分信息。此为该协议的能力缺口，网关无法凭空推断，
  故**不实现**（避免伪造）。当前恒返回 `"stop"` 属如实映射。

### Added
- 配置项 `connect_timeout_secs`（默认 20）；环境变量 `HTTP_TIMEOUT_SECS` / `CONNECT_TIMEOUT_SECS`。
- 测试：`breaker_opens_after_threshold`、`breaker_half_open_allows_single_probe_m9`、
  `breaker_half_open_failure_reopens_immediately_m9`、`breaker_success_closes`、
  `m2_stream_client_has_no_total_timeout`、`m5_nonce_failure_detection`、
  `m8_needs_prefetch_true_when_near_expiry`。

### Verified
- `cargo test --all`：**101 单测 + 33 集成全绿**（较 v0.5.0 新增 7 单测）；
  `cargo fmt --all -- --check` 与 `cargo clippy --all-targets -- -D warnings` 均干净。
- **真实 E2E**（连真实上游 deepseek.es + cf_solver）：v0.5.0 的 8/8 全部保持通过
  （H3/M4/M7/伪工具/OpenAI 流式），证明可靠性改动未破坏主路径。
- **M8 真实验证**（`cookie_ttl_secs=60` 短 TTL）：日志出现「cookie 临近过期，后台预取续期（M8）」，
  预取后请求仍返回 200——后台预取真实生效。
- **压测**：8 并发 × 24 请求 = **100% 成功**，p50=2012ms、p99=3232ms、QPS 4.07。

---

## [0.5.0] - 2026-10-07


**v1.0.0 正确性加固批次收尾**：关闭全部剩余 HIGH（H3/H4）与 M1/M3/M4/M7/M11/M12。
至此 §2.1 的四项 HIGH **全部闭环**，网关达到「正确性基线」。

### Fixed
- **HIGH · Anthropic 端点错误体是 OpenAI 结构（H3）**：`errors.rs` 的 `IntoResponse` 是全局唯一的，
  `/v1/messages` 与 `/v1/messages/count_tokens` 的所有错误（401/400/429/502…）都返回
  `{"error":{...}}`。Anthropic 规范要求顶层 `{"type":"error","error":{"type":..,"message":..}}`，
  否则 Claude Code / Anthropic SDK 解析错误失败。
  修复：新增 `AppError::anthropic_error_type()`（把无对应类型的 `upstream_error`/`network_error`/
  `internal_error` 收敛为合法的 `api_error`）与 `into_anthropic_response()`；两个 Anthropic 处理器
  统一经此转换。**OpenAI 端点结构不变**（有回归测试守护）。
- **HIGH · 伪工具说明从未注入（H4）**：`features::TOOL_INSTRUCTION` 已定义但**全仓零引用**，
  模型永远收不到使用说明 → 伪工具功能实际不可用。
  修复：新增 `features::inject_prompt_prefixes()`（语言指令 + 工具说明组合注入），
  新增配置 `pseudo_tools_enabled`（默认 false，避免改变现有行为）；OpenAI 与 Anthropic 两端均接线。
  删除已无生产调用点的旧 `inject_system_prompt`（避免新的死代码）。
- **MEDIUM · 哨兵值 `__TS_REQUIRED__` 泄漏下游（M3）**：翻译层返回字符串哨兵，
  中途触发或重试失败时原样进入下游 `message` 字段；Anthropic 非流式甚至不特判。
  修复：新增结构化枚举 `Translated::TsRequired`，全路径（OpenAI/Anthropic × 流式/非流式）
  统一处理，杜绝哨兵字符串外泄。
- **MEDIUM · Anthropic 流式 usage 恒为 0/0（M4）**：`message_start`/`message_delta` 硬编码 0。
  修复：`message_start` 填 `estimate_tokens(prompt)`；`message_delta` 填累积输出估算。
- **MEDIUM · 上游中途断开时结束信号不完整（M7）**：OpenAI 流无条件追加 `[DONE]` 但缺 `finish_reason`；
  Anthropic 流断开时无 `message_stop`，客户端视为异常截断。
  修复：两协议均在流尾检测「未收到 Done」时补发标准结束序列。
- **MEDIUM · Anthropic 非流式无响应上限（M11）**：`max_response_bytes` 仅约束 OpenAI 非流式。
  修复：Anthropic 非流式聚合同步加上限保护。
- **MEDIUM · 未识别上游事件静默丢弃（M12）**：`translate_event` 的 `_ => None` 无任何可观测痕迹。
  修复：对未知命名事件记 `debug` 日志（保留 `message_start` 静默），便于未来上游新增事件时排查。

### Added
- 配置项 `pseudo_tools_enabled`（伪工具说明注入开关）。
- 测试：`anthropic_error_type_only_uses_valid_values`、`translate_unknown_event_ignored_m12`、
  `tool_instruction_injected_when_enabled`、`tool_instruction_absent_when_disabled`、
  `language_and_tools_both_injected`、`no_prefixes_is_noop`；
  集成 `anthropic_error_body_is_anthropic_shaped`、`anthropic_bad_request_is_anthropic_shaped`、
  `openai_error_body_stays_openai_shaped`、`anthropic_stream_usage_is_nonzero`、
  `openai_stream_abrupt_close_has_finish_reason`、`anthropic_stream_abrupt_close_has_message_stop`、
  `anthropic_nonstream_respects_max_response_bytes`、`tool_instruction_reaches_upstream_when_enabled`。
- 脚本 `scripts/e2e-v1.mjs`（真实 E2E：H3/M4/M7/H4 + OpenAI 回归）。

### Verified
- `cargo test --all`：**94 单测 + 33 集成全绿**（较 v0.4.0 新增 5 单测 + 8 集成）；
  `cargo fmt --all -- --check` 与 `cargo clippy --all-targets -- -D warnings` 均干净。
- **真实 E2E**（连真实上游 deepseek.es + cf_solver）：**8/8 通过**——
  Anthropic 错误体为 Anthropic 结构；流式 `usage.output_tokens > 0`；正常结束含 `message_stop`；
  OpenAI 流式中文无 `�` 且含 `finish_reason` + `[DONE]`；
  **伪工具真实生效**：模型实际输出 ` ```tool ` 块并触发本地执行
  （`[tool:get_time] {"ok":true,"result":"2026-10-07T03:54:02Z"}`）。
- **压测**：8 并发 × 24 请求 = **100% 成功**，p50=2019ms、p99=3861ms、QPS 4.07。

---

## [0.4.0] - 2026-10-07


两项 **HIGH 级正确性修复**，均由独立审计发现、经真实 E2E 反证确认、并按 TDD 流程修复。
其中 H1 是「唯一会造成**静默内容损坏**」的缺陷。

### Fixed
- **HIGH · SSE 多字节字符静默损坏（H1）**：`parse_sse_stream` 原用
  `String::from_utf8_lossy(&b)` 直接解码每个网络 chunk。chunk 边界是任意的，
  可能落在 UTF-8 码点中间——被截断的半个码点被替换为 U+FFFD，下一 chunk 的前导续字节
  同样被替换，**一个 CJK/emoji 字符损坏成两个替换符**。上游为西语站点但常服务中文用户，
  属高概率事件。
  修复：改为**字节级缓冲**（`Vec<u8>`），只在完整事件边界解码；不完整尾字节留在缓冲等待补齐。
  同时支持 SSE 规范允许的 `\r\n\r\n` / `\r\r` 分隔符（M1），并新增 8MB 缓冲上限保护
  （无分隔符的超大流判定异常而非无界增长）。
  反证：旧实现下 `漢字テスト😀` → `??????????`；修复后完整保留。
- **HIGH · 多轮对话历史被整体丢弃（H2）**：`messages_to_prompt` 只取最后一条 user，
  历史交给上游 `conversation_uuid` 承接。但标准 OpenAI 客户端**每轮发送完整历史**且
  多数 SDK 默认不设 `user` 字段 → 网关每次生成随机 conv_uuid → 上游视为全新会话 →
  **整个历史被静默丢弃**，模型只看到最后一句话。Anthropic 侧有相同缺陷。
  修复：多轮请求渲染**完整转录**（带 role 标注），单轮保持原样（向后兼容）。
  反证：旧实现下第 2 轮回答「我无法看到你之前的消息，所以不知道你最喜欢的颜色是什么」；
  修复后正确回答「紫色」。

### Added
- 回归测试：`sse_multibyte_split_across_chunks`、`sse_multibyte_split_at_every_boundary`、
  `sse_partial_utf8_left_in_buffer_no_replacement`、`sse_crlf_block_separator`、
  `sse_oversized_buffer_emits_error_not_oom`（`src/upstream.rs`）；
  `multi_turn_history_preserved`、`multi_turn_roles_labeled`、`single_turn_unchanged_shape`、
  `system_plus_multi_turn`（`src/protocol/openai.rs`）；
  `anthropic_multi_turn_history_preserved`、`anthropic_single_turn_unchanged`、
  `anthropic_missing_user_errors`（`src/protocol/anthropic.rs`）。
- 集成测试：`multi_turn_history_reaches_upstream`（断言多轮历史真实送达上游）、
  `multibyte_content_survives_gateway`（断言端到端无 U+FFFD）。
- 脚本：`scripts/e2e-multiturn.mjs`（真实多轮 E2E）、`scripts/e2e-diagnose.mjs`（SSE 诊断）。

### Verified
- `cargo test --all`：**89 单测 + 25 集成全绿**（较 v0.3.1 新增 12 单测 + 2 集成）；
  `cargo fmt --all -- --check` 与 `cargo clippy --all-targets -- -D warnings` 均干净。
- **真实 E2E**（连真实上游 deepseek.es + cf_solver）：
  - H1：中文流式回复 `1+1 等于 **2**。` 完整无 `�`；16 并发中文流式请求 **0 个替换符**。
  - H2：无 `user` 字段的两轮对话，模型准确回忆起第 1 轮内容（回答「紫色」）。
  - 反证实验：临时恢复旧代码后，H1 输出替换符、H2 回答「无法看到之前的消息」——
    证明两项修复解决的是**真实缺陷**而非理论问题。
- **压测**：8 并发 × 24 请求 = **100% 成功**，p50=1721ms、p99=2783ms、QPS 5.25；
  32 并发 × 64 请求 = **100% 成功**，p50=210ms、p99=328ms、QPS 138.83。

---

## [0.3.1] - 2026-10-06


独立审计（P3 新增模块）后修复 5 项——其中 2 项为「宣称有实现但实际不可达」的假实现，
与项目「真实闭环」契约冲突，属必须修复。

### Fixed
- **HIGH · 断线重放不可达（假实现）**：`ReplayStore::replay()` / `parse_last_event_id()`
  仅在 `replay.rs` 自身单测中被调用，`src/` 中**零生产调用点**——宣称的「断线重放」实际无法触发。
  修复：新增 `GET /v1/responses/{id}` 端点；流式响应返回 `x-response-id` 头；
  客户端凭该 id + `Last-Event-ID` / `?after=` 即可回放。未知/过期 id → 409。
  新增集成测试 `replay_endpoint_is_reachable` 锁定可达性。
- **HIGH · 用量漏记主路径**：账本仅在「非流式 OpenAI」与「缓存命中」处记录，
  **流式请求与全部 Anthropic 请求不入账**，控制台用量严重低估。
  修复：OpenAI 流式（流结束时记，含 completion token 估算）、Anthropic 流式与非流式
  全部接入账本。新增测试 `streaming_requests_are_recorded`、`anthropic_requests_are_recorded`。
- **MEDIUM · 账本打开失败阻断主服务**：`Ledger::open` 失败会 fail-fast 让网关无法启动，
  但账本是旁路观测组件。修复：降级为内存模式 + warn，不阻断（测试 `open_failure_degrades_not_fatal`）。
- **MEDIUM · 重放 seq 超限后钉死**：`seq = entries.len()+1` 在超过 `max_entries` 后不再递增，
  破坏 SSE `last-event-id` 语义。改用独立单调计数器（测试 `seq_stays_monotonic_after_cap`）。
- **LOW · 密钥脱敏不足**：`key_id` 保留前 6 位、熵偏高。改为前 4 + 末 2 + 长度；
  过短密钥不暴露任何字符（测试 `key_id_masks_secret` / `key_id_short_key_hides_all`）。

### Verified
- `cargo test --all`：**77 单测 + 23 集成全绿**；fmt / `clippy -D warnings` 干净。
- **真实 E2E**：流式响应带 `x-response-id`；`GET /v1/responses/{id}?after=2` 真实回放 seq 3+
  （8 个事件）；未知 id → 409；账本记录 `stream:true, prompt_tokens:4, completion_tokens:8`；
  Anthropic 请求入账（total 1→2）。

## [0.3.0] - 2026-10-06

P3 增强全部落地：控制台、响应缓存、用量账本、多 solver 负载均衡、断线重放、语言注入、伪工具。

### Added
- **控制台 Web UI（P3-1）**：`/admin` 单页（自包含、无 CDN、深色编辑台风格）+ `/admin/api/status`。
  展示模型、实时用量、solver 健康、最近请求、配置概览。默认关闭；需 `admin_enabled=true` + `admin_token`。
  令牌校验用常量时间比较；未启用返回 404、令牌缺失/错误 401。
- **请求级响应缓存（P3-2）**：`cache.rs`，LRU + TTL。仅对**无会话**请求启用（避免与上游按
  `conversation_uuid` 维护的多轮上下文冲突）；命中不打上游。
- **用量账本（P3-3）**：`ledger.rs`，SQLite（rusqlite bundled）持久化；`spawn_blocking` 写入不阻塞
  运行时，失败仅告警不阻断请求。查询聚合统计与最近记录；`key_id` 脱敏（仅留前 6 位）。
- **多 cf_solver 负载均衡（P3-4）**：`solver.rs` 改 `SolverPool`——轮询选取、跳过连续失败 ≥3 的实例、
  单实例失败即故障转移到下一个；`health_snapshot()` 供控制台。
- **断线重放（P3-5）**：`replay.rs`。为下游 SSE 事件编递增 `id:`，内存缓冲支持同进程内回放。
  **限制如实披露**：上游不提供 `Last-Event-ID`（SSE 无 `id:` 行）且 `cache_key` 一次性，
  故无法实现真正的上游续传。
- **语言注入（P3-6）**：`features::inject_system_prompt`；`system_prompt_suffix` 追加 system 指令
  （实测：向默认西语站点注入"Always answer in English"后，真实上游改为英文回复）。
- **伪工具调用（P3-7）**：`features::parse_tool_calls` 提取 ` ```tool ` JSON 块，网关本地执行
  （内置 `get_time`/`echo`）并把结果回填到回复。
- 非流式聚合输出上限 `max_response_bytes`。

### Changed
- `AppState` 新增 `cache`/`ledger`/`replay` 字段；`build_router` 注册 `/admin` 路由。

### Verified
- `cargo test --all`：**73 单测 + 20 集成全绿**；fmt / clippy `-D warnings` 干净；doctest 通过。
- **真实 E2E**（deepseek.es + cf_solver）：
  - 控制台：正确令牌 200（6KB HTML）、错误令牌 401、未启用 404。
  - 缓存：相同请求第二次命中（账本 `cache_hits=1`）。
  - 账本：`total_requests=3`、`avg_latency_ms` 等正确；SQLite 文件持久化。
  - solver 健康：`healthy=true`、`last_success_secs≈26.4`。
  - **多 solver 故障转移**：配置 `["http://127.0.0.1:9999"(坏), "http://127.0.0.1:8001"(好)]`，
    请求自动跳过坏实例、经好实例成功。
  - **语言注入**：中文提问 + 英文指令 → 真实上游返回英文 `"Hello! How can I help you today?"`。
  - **SSE id**：流式事件带递增 `id: 1..8` + `[DONE]`。

## [0.2.1] - 2026-10-06

独立代码审计（v0.1.0..v0.2.0 全量 diff，结论 APPROVE/无 CRITICAL-HIGH）后修复 2 MEDIUM + 4 LOW。

### Fixed
- **并发限制语义**：`max_concurrency` 改用 `GlobalConcurrencyLimitLayer`（全局共享信号量）。原 `ConcurrencyLimitLayer` 是 per-route——4 个业务路由各持独立信号量，`max=4` 实际可放行 16，与「最大并发请求数」不符。
- **退避溢出**：`solve_with_retry` 的 `1u64 << i` 在 `solver_retries >= 64` 时于 debug 构建触发 `attempt to shift left with overflow` panic。改为 `1u64 << i.min(3)`（先夹指数再移位）。
- **熔断半开**：冷却到点进入半开时重置 `consecutive_failures`，避免失败计数无限累积与「冷却后一失败即重开」。
- **死配置 `default_model`**：此前被解析但从不读取，现接线到 `resolve_model`（`DEFAULT_MODEL` 环境变量/配置真正生效）。
- 清理 e2e 脚本中已删除的 `pseudo_chunk_chars` 字段。

### Changed
- README 澄清：`max_concurrency` 为**全局**且仅在 handler 返回响应头前生效（不约束已建立的 SSE 流时长）；`rate_limit_per_sec` 位于鉴权之前、无 per-client 维度。

### Verified
- `cargo test --all`：48 单测 + 15 集成全绿（新增 `default_model_override_takes_effect`）。
- 实测：限流 `5/s` 突发 30 → `200×5 + 429×25`；`/healthz` 全豁免；并发 64 → 100% 成功；退避算法 `i<200` 无溢出。

## [0.2.0] - 2026-10-06

### Fixed
- **SSE 解析丢事件（严重回归）**：`parse_sse_stream` 重写为 `unfold` 后，在 `await` 前未先排空缓冲，导致**单个 chunk 内含多个事件时只产出前两个**，后续 delta 全部丢弃（表现为网关返回空回复）。修复：每轮迭代先 drain 缓冲区完整块，再读取下一个 chunk。新增回归测试 `sse_single_chunk_multiple_events`。
- **安全（P0）**：启动时安全校验——监听地址为非回环（如 Docker `0.0.0.0`）且未配置 `api_keys` 时**拒绝启动**（fail-fast + 中文错误），避免无鉴权开放代理；可用 `ALLOW_INSECURE_PUBLIC=1` 显式放开。
- **认证自愈（P1）**：真正实现 `ts_required` 自动恢复。此前流式路径会把 `__TS_REQUIRED__` 当文本塞进流（客户端解析失败）、非流式直接 502 且不重试。现改为：探测上游**首个事件**，若为 `ts_required` 则 `force_reauth()`（清 cookie/nonce 强制重新求解）后**重试一次**（重试时换新响应 id）；仅在未向下游输出前触发，无重复内容风险。
  - 新增 `UpstreamClient::force_reauth()`。
  - `src/api.rs` 的 `start_stream` 拆分为「首事件探测+重试」与 `start_stream_once`。
- **并发认证连坐失败（P1）**：`ensure_authed` 改用 `tokio::sync::Mutex` 串行锁，等待者拿到锁后重新检查状态——前一次求解失败则自己接手，不再直接返回错误。
- **配额耗尽未映射 429（P1）**：`translate_event` 解析 `quota_notice` → `Translated::Quota` → 非流式 `AppError::QuotaExhausted`（429）/ 流式 `rate_limit_error` 错误帧。
- **模型路由"假实现"（P1）**：`/v1/models` 由 6 个虚构 provider 别名收敛为**唯一真实模型** `deepseek-es`（`routable: true`）；历史别名仍被接受但不再列出。新增 `alias_of`/`routable` 字段如实标注。
- 修 `x-requested-with` 头构造后被丢弃的死代码（现真正发送 `XMLHttpRequest`）。

### Added
- **弹性层（P1）**：solver 求解失败指数退避重试（`solver_retries`，默认 2）；认证熔断器（`breaker_fail_threshold`/`breaker_cooldown_secs`，连续失败后短时快速失败）；端点限流（`rate_limit_per_sec`，固定窗口）与有界并发（`max_concurrency`，`ConcurrencyLimitLayer`）。
- **优雅关闭（P2）**：`with_graceful_shutdown`，SIGINT/SIGTERM 后在途请求完成再退出。
- 会话表容量上限（`MAX_SESSIONS=10_000`，超限淘汰最久未活跃一半）。
- CJK 感知 token 估算（中文约 1 token/字，替代 `chars/4` 的严重低估）。
- OpenAI 流首帧补 `role` chunk（对齐规范）。
- 清理死依赖 `regex`/`rand`/`once_cell` 与死配置 `pseudo_chunk_chars`。
- 单测：`reject_public_bind_without_keys`、`allow_public_bind_with_keys`、`allow_loopback_without_keys`、`explicit_escape_hatch_allows_public`、`loopback_detection_helper`、`catalog_single_routable_model`、`legacy_aliases_still_accepted`、`evicts_when_full`、`translate_quota_notice`、`translate_quota_without_error_field`、`sse_flush_trailing_without_blank_line`、`sse_single_chunk_multiple_events`。
- 集成测试：`ts_required_triggers_reauth_and_retry`、`quota_exhausted_returns_429`、`solver_retry_recovers_from_transient_failure`、`circuit_breaker_opens_after_failures`、`rate_limit_returns_429`、`models_single_routable`。

### Verified
- `cargo test --all`：**47 单测 + 15 集成全绿**。
- `cargo fmt --all -- --check`、`cargo clippy --all-targets -- -D warnings` 均通过。
- 手工实测 4 场景：`0.0.0.0` 无 key → 退出码 1；`0.0.0.0`+key / 回环 / 逃生阀 → `/healthz` 200。
- **真实 E2E**（`scripts/e2e-gateway.mjs`，真实 deepseek.es + 真实 cf_solver）：`模型数: 1`；OpenAI 流式（6 chunks）+ 非流式 + Anthropic 事件序列完整；真实回复 `¡Hola! Espero que tengas un día maravilloso.`。
- **真实自愈 E2E**（`scripts/e2e-selfheal.mjs`）：首帧注入真实抓包的 `ts_required` → 日志「上游要求重新安全校验」→ `force_reauth` 真实重新求解 → 真实兑换 → 重试 → 真实回复 `¡Hola! 😊 ¿En qué puedo ayudarte hoy?`（HTTP 200）。
- **真实压测**：8 并发 × 24 请求 → **24/24 成功（100%）**，QPS 3.53，p50=1965ms / p90=2868ms / p99=3081ms。
- 上游日志可见求解重试生效：`求解尝试 1 失败` → `求解成功（token 730 字符）`。

## [0.1.0] - 2026-10-06

### Added
- 初始版本：deepseek.es → OpenAI/Anthropic 兼容 API 网关（Rust）
- **认证链路**：Cloudflare Turnstile 求解（对接 cf_solver）→ 安全 Cookie → nonce
- **聊天链路**：`aipkit_cache_sse_message` → `aipkit_frontend_chat_stream`（SSE）
- **OpenAI 兼容**：`/v1/chat/completions`（流式 + 非流式）、`/v1/models`
- **Anthropic 兼容**：`/v1/messages`（流式 + 非流式）、`/v1/messages/count_tokens`
- **会话映射**：下游稳定 key → 上游 `conversation_uuid`（确定性派生，支持多轮上下文）
- **认证缓存 + 自愈**：cookie TTL 缓存 + 并发双检锁（避免重复求解）+ `ts_required` 自动重新求解
- **下游鉴权**：Bearer / x-api-key 白名单（常量时间比较）
- **HTTP 客户端**：手动 cookie jar、可选出口代理、SSE 规范解析（空行分块）
- **集成 cf_solver**：`tools/cf_solver/`（camoufox 浏览器求解，源自 imagefree-2ai）
- **测试**：37 单元测试 + 9 集成测试（mock 上游，无真实网络依赖）
- **E2E 脚本**：`scripts/e2e-gateway.mjs`（真实上游）、`scripts/loadtest.mjs`（压测）
- **文档**：`分析文档/00-09`（完整逆向分析 + E2E 验证报告）、`docs/DEPLOY.md`、`docs/PROTOCOL.md`
- **CI**：GitHub Actions（fmt + clippy + test + docker build）
- **Docker**：多阶段构建

### 验证（真实上游）
- OpenAI 流式/非流式、Anthropic 流式/非流式全部通过
- 压测 8 并发 × 24 请求：100% 成功，p50=1.86s，QPS 3.62
