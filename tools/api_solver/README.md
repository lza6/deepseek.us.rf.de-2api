# api_solver — 第三方 captcha API 适配器（纯 HTTP，无浏览器）

把 [capsolver](https://capsolver.com) / [2captcha](https://2captcha.com) 的 Turnstile 求解 API
**翻译成本项目网关认识的 `cf_solver` 契约**。

## 为什么

默认的 `tools/cf_solver`（camoufox 浏览器）有两类问题：
1. **单点故障**：浏览器起不来就全停（浏览器损坏、系统资源不足、**网络出口异常**）；
2. **部署重**：需要图形环境与较多资源。

本适配器是**纯 HTTP** 后端，网关无需改动即可用（`solver_urls` 直接指向它），
并可与 camoufox 实例**并存做故障转移**。

## 用法

```bash
# 启动适配器（capsolver 为例）
API_SOLVER_KEY=<你的 capsolver key> node tools/api_solver/server.mjs
# → 监听 0.0.0.0:8002

# 网关 config.json：camoufox 为主，API 为备（任一可用即不中断）
{
  "solver_urls": ["http://127.0.0.1:8001", "http://127.0.0.1:8002"]
}
```

切到 2captcha：
```bash
API_SOLVER_PROVIDER=2captcha API_SOLVER_KEY=<key> node tools/api_solver/server.mjs
```

## 环境变量

| 变量 | 默认 | 说明 |
|------|------|------|
| `API_SOLVER_PROVIDER` | `capsolver` | `capsolver` 或 `2captcha` |
| `API_SOLVER_KEY` | （空） | 第三方 API key；**未配置时 `/turnstile` 返回 503** |
| `API_SOLVER_PORT` | `8002` | 监听端口（避开 camoufox 的 8001） |
| `API_SOLVER_POLL_MS` | `3000` | 轮询间隔（说明用；实际由网关侧轮询驱动） |
| `API_SOLVER_TIMEOUT_MS` | `120000` | 单任务超时 |

## 契约（与 `src/solver.rs` 完全一致）

```
GET /turnstile?url=&sitekey=[&action=]  → 202 {task_id, status:"accepted"}
GET /result?id=<task_id>                → 200 {status:"success", value:<token>}
                                        → 200 {status:"process"}   求解中
                                        → 200 {status:"error", message}
                                        → 404 未知/过期
GET /health                             → 200 {status:"ok", backend, key_configured, pending}
```

## 测试（无需网络/无需 key）

```bash
node tools/api_solver/server.test.mjs
```

## 说明（如实）

- 这是**第三方付费服务**的适配器；本项目**不含任何 key**，也不代付费用。
- 未配置 key 时服务可正常启动，但求解请求返回 503（不会静默失败）。
- **Turnstile Managed Challenge 无法纯协议伪造**（见 `docs/TROUBLESHOOTING-NETWORK.md`），
  本适配器走的是第三方**真实求解**（其内部仍是浏览器农场），而非协议伪造。
