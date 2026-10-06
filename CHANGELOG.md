# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/) 与 [语义化版本](https://semver.org/)。

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
- OpenWA 流式/非流式、Anthropic 流式/非流式全部通过
- 压测 8 并发 × 24 请求：100% 成功，p50=1.86s，QPS 3.62
