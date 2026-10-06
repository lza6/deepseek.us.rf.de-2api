# deepseek-es-2api

把 [deepseek.es](https://deepseek.es)（WordPress + AIPKit 聊天）转换为 **OpenAI 兼容**（`/v1/chat/completions`）
与 **Anthropic 兼容**（`/v1/messages`）的 API 网关，内置 **Cloudflare Turnstile 求解**。

> 上游为非标准 SSE 协议，本网关在 Rust 侧完成认证（Turnstile）、会话管理与协议翻译。
> **已通过真实 E2E 验证**：Turnstile 求解 → 安全 Cookie → SSE 流 → OpenAI/Anthropic 兼容输出。

---

## 架构

```
┌─────────────┐   OpenAI/Anthropic    ┌──────────────────────────┐
│  客户端      │ ───────────────────▶ │  deepseek-es-2api (Rust)  │
│ (Cursor/    │                       │  · 协议翻译 (SSE↔OpenAI)  │
│  Claude Code)│ ◀─────────────────── │  · 会话映射 (conv_uuid)   │
└─────────────┘     SSE / JSON        │  · Cookie 池 + 自愈       │
                                      └───────────┬──────────────┘
                                                  │
                          ┌───────────────────────┼───────────────────────┐
                          ▼                       ▼                       ▼
                  ┌───────────────┐      ┌────────────────┐     ┌─────────────────┐
                  │ deepseek.es   │      │ cf_solver      │     │ Turnstile       │
                  │ admin-ajax    │      │ (camoufox)     │     │ 求解            │
                  │ + SSE         │      │ 127.0.0.1:8001 │     │                 │
                  └───────────────┘      └────────────────┘     └─────────────────┘
```

**认证流程**（首次/失效时自动触发）：
```
solve Turnstile (cf_solver) → POST deepseek_ts_verify → dsts_ok cookie
  → POST aipkit_get_frontend_chat_nonce → nonce
  → POST aipkit_cache_sse_message → cache_key
  → GET  aipkit_frontend_chat_stream (SSE) → message_start/delta/done
```

---

## 快速开始

### 1. 启动 cf_solver（Cloudflare Turnstile 求解）

需要 Python 3.11 + camoufox（见 `tools/cf_solver/requirements.txt`）：

```bash
cd tools/cf_solver
pip install -r requirements.txt
python -m camoufox fetch          # 首次下载浏览器
python boterdrop_wrapper.py       # 监听 0.0.0.0:8001，等待 "Pool siap"
```

### 2. 编译并启动网关

```bash
cargo build --release
cp config.example.json config.json   # 按需修改
./target/release/deepseek-es-2api --config config.json
```

默认监听 `http://127.0.0.1:47833`。

### 3. 接入客户端

**OpenAI SDK / Cursor / Continue**
```
Base URL: http://127.0.0.1:47833/v1
API Key:  sk-local（未配置 api_keys 时随意填）
Model:    deepseek-es
```

**Claude Code（Anthropic 协议）**
```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:47833
export ANTHROPIC_API_KEY=sk-local
claude
```

---

## API 端点

| 端点 | 方法 | 说明 |
|------|------|------|
| `/v1/chat/completions` | POST | OpenAI 聊天（流式 / 非流式） |
| `/v1/messages` | POST | Anthropic 聊天（流式 / 非流式），供 Claude Code |
| `/v1/messages/count_tokens` | POST | token 预估 |
| `/v1/models` | GET | 模型列表 |
| `/healthz` | GET | 健康检查 |
| `/admin` | GET | 控制台 Web UI（需 `admin_enabled` + `admin_token`） |
| `/admin/api/status` | GET | 控制台数据 JSON |
| `/v1/responses/{id}` | GET | **断线重放**：回放某次流式响应未收到的事件 |

### 控制台

`admin_enabled=true` 且设置 `admin_token` 后访问 `http://127.0.0.1:47833/admin?token=<token>`
（或 `X-Admin-Token` 头）。展示：模型列表、实时用量统计、solver 健康、最近请求、配置概览。
自包含单页（无外部 CDN），仅本机建议开启。

### 增强特性

| 特性 | 配置 | 说明 |
|------|------|------|
| **请求级响应缓存** | `cache_ttl_secs` | 相同 prompt（**无会话语义**）短时复用，降低上游压力 |
| **用量账本** | `ledger_path` | SQLite 持久化（`usage.db`）；空 = 关闭 |
| **多 solver 负载均衡** | `solver_urls` | 轮询 + 健康探测 + 故障转移；空则用 `cf_solver_url` |
| **语言注入** | `system_prompt_suffix` | 追加 system 指令（如"用用户语言回答"） |
| **伪工具调用** | — | 模型输出 ` ```tool ` JSON 块 → 网关本地执行并回填 |
| **断线重放** | — | SSE 事件带 `id:`；见下方限制说明 |

> **断线重连限制（如实披露）**：上游 SSE **不发 `id:` 行**且 `cache_key` 一次性，
> 故**无法**实现真正的 `Last-Event-ID` 上游续传。网关侧实现的是**缓冲重放**：
> 流式响应带 `x-response-id` 头，客户端断线后可 `GET /v1/responses/<id>` 并附
> `Last-Event-ID`（或 `?after=N`）**在同进程内重新获取**未收到的事件；
> 进程重启或缓冲过期（默认 TTL 300s）返回 409，需客户端重新发起。

---

## 配置（`config.json`）

| 字段 | 默认 | 说明 |
|------|------|------|
| `listen_addr` | `127.0.0.1:47833` | 监听地址 |
| `upstream_base_url` | `https://deepseek.es` | 上游 |
| `bot_id` | `27623` | AIPKit 机器人 ID |
| `sitekey` | `0x4AAAAAADlLZ3ljqZP6cQwq` | Turnstile sitekey |
| `cf_solver_url` | `http://127.0.0.1:8001` | 求解器地址 |
| `api_keys` | `[]` | 下游 Key；空 = 仅本机放行 |
| `proxy` | `null` | 上游出口代理（如 `http://127.0.0.1:10808`） |
| `cookie_ttl_secs` | `1800` | 安全 cookie 缓存 TTL |
| `cors_allow_origins` | `[]` | CORS；空 = 关闭 |
| `solver_retries` | `2` | 求解失败重试次数（指数退避） |
| `breaker_fail_threshold` | `5` | 认证熔断：连续失败阈值 |
| `breaker_cooldown_secs` | `30` | 认证熔断：冷却秒数 |
| `max_concurrency` | `0` | **全局**最大并发（0 = 不限）。注：仅在 handler 返回响应头前生效，不约束已建立的 SSE 流时长 |
| `rate_limit_per_sec` | `0` | 端点每秒限流（0 = 不限，固定窗口，全局共享；位于鉴权之前，未鉴权请求同样计数） |
| `admin_enabled` | `false` | 启用控制台 |
| `admin_token` | `""` | 控制台访问令牌（启用时必填） |
| `cache_ttl_secs` | `300` | 响应缓存 TTL（0 = 关闭） |
| `cache_max_entries` | `1000` | 响应缓存最大条目 |
| `cache_min_chars` | `0` | 低于该长度的结果不缓存 |
| `ledger_path` | `usage.db` | SQLite 账本路径（空 = 关闭） |
| `solver_urls` | `[]` | 多 cf_solver 地址（空则用 `cf_solver_url`） |
| `system_prompt_suffix` | `""` | 追加的 system 指令（语言/风格） |
| `max_response_bytes` | `2097152` | 非流式聚合上限（防超大响应） |

环境变量可覆盖：`LISTEN_ADDR` `UPSTREAM_BASE_URL` `BOT_ID` `SITEKEY` `CF_SOLVER_URL` `API_KEYS` `PROXY` `DEFAULT_MODEL` `CORS_ALLOW_ORIGINS`

> **安全**：监听地址为**非回环**（如 `0.0.0.0`）且 `api_keys` 为空时，网关**拒绝启动**（fail-fast）。确需无鉴权暴露公网须显式设置 `ALLOW_INSECURE_PUBLIC=1`（危险）。

---

## 模型

上游前端不暴露真实模型 ID（由服务端 bot 配置决定）。本站实测为**单一 bot**（`bot_id=27623`，`provider=DeepSeek`），代理**无法切换 provider**。

因此本网关**只暴露一个真实模型**：

| 模型 id | 说明 |
|---------|------|
| `deepseek-es` | 唯一真实模型（`routable: true`） |

> 历史别名（`deepseek-es-openai` / `-claude` / `-google` / `-openrouter` / `-xai`）仍被接受以兼容旧客户端，但**不再是独立模型**——会解析到 `deepseek-es`，且不再出现在 `/v1/models`。`/v1/models` 条目含 `routable` 与 `alias_of` 字段以如实标注。

---

## 开发与测试

```bash
# 单元测试 + 集成测试（mock 上游，46 项）
cargo test

# 格式与静态检查
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings

# 真实 E2E（需 cf_solver 运行）
node scripts/e2e-gateway.mjs "Di hola en una frase"

# 压测
node scripts/loadtest.mjs 4 12
```

**实测结果**（真实上游）：
| 指标 | 值 |
|------|-----|
| 单测 + 集成 | 46/46 通过 |
| E2E OpenAI 流式 | ✅ 真实回复 |
| E2E Anthropic 流式 | ✅ 事件序列完整 |
| 压测 8 并发 × 24 请求 | 100% 成功，p50=1.86s，QPS 3.62 |

---

## 深度分析文档

`分析文档/` 内含完整逆向分析（00-09），包括上游协议、SSE 事件、配置结构、配额机制、E2E 验证报告。

---

## 免责声明

本项目仅用于**学习与研究**。请遵守上游服务条款，不得用于滥用或绕过配额牟利。
