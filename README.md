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

环境变量可覆盖：`LISTEN_ADDR` `UPSTREAM_BASE_URL` `BOT_ID` `SITEKEY` `CF_SOLVER_URL` `API_KEYS` `PROXY` `DEFAULT_MODEL` `CORS_ALLOW_ORIGINS`

---

## 模型

上游前端不暴露真实模型 ID（由服务端 bot 配置决定），本网关暴露 **provider 级别名**：

| 模型 id | 说明 |
|---------|------|
| `deepseek-es` | 默认（上游 bot 配置的 provider=DeepSeek） |
| `deepseek-es-openai` / `-claude` / `-google` / `-openrouter` / `-xai` | 按 provider 路由 |

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
