# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/) 与 [语义化版本](https://semver.org/)。

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
