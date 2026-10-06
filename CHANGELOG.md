# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/) 与 [语义化版本](https://semver.org/)。

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
