# SOP · 运维标准作业流程

> deepseek-es-2api 网关的日常运维与故障处置。**交接必读**。

## 1. 启动

```bash
# 1) 启动 cf_solver（Python + camoufox，端口 8001）
cd tools/cf_solver
python boterdrop_wrapper.py     # 等 "Pool siap"

# 2) 启动网关
cp config.example.json config.json   # 按需改
./target/release/deepseek-es-2api --config config.json
```

**就绪判据**：`curl http://127.0.0.1:47833/healthz` 返回 `{"status":"ok",...}`。

## 2. 健康检查

| 检查 | 命令 | 期望 |
|------|------|------|
| 网关存活 | `curl -s /healthz` | `status:ok` |
| 求解器健康 | `curl -s /admin/api/status`（需 token） | `solver[].healthy` |
| 认证状态 | 同上 | `upstream.authed:true` |
| 用量 | 同上 | `stats.total_requests` 增长 |

## 3. 故障处置

| 症状 | 定位 | 处置 |
|------|------|------|
| 502 `SolverFailed` | cf_solver 挂了/重启窗口（每 10min 重启 ~30s） | 等 30s 重试；或配 `solver_urls` 多实例 |
| 502 `认证熔断中` | 连续认证失败达阈值 | 检查 cf_solver；等 `breaker_cooldown_secs` 冷却 |
| 429 | 上游配额耗尽 | 查 `/v1/balance`；等配额重置（日限） |
| 流式中断无 `[DONE]` | 上游断开 | 用 `x-response-id` + `GET /v1/responses/{id}` 重放 |
| 中文变 `?` | **不应发生**（v0.4.0 已修 H1）；若发生说明版本过旧 | 升级到 ≥v0.4.0 |
| 启动即退出 | `validate_security` fail-fast（非回环+空 keys） | 设 `api_keys` 或 `ALLOW_INSECURE_PUBLIC=1`（危险） |

## 4. 配置要点

| 配置 | 生产建议 |
|------|---------|
| `api_keys` | **必填**（公网部署时） |
| `listen_addr` | 默认 `127.0.0.1`；公网须配 keys |
| `admin_token` | 启用控制台时**必填**（明文比对，注意保密） |
| `cookie_ttl_secs` | 1800（剩余 20% 自动预取续期） |
| `max_concurrency` | 按机器规格设（0=不限） |
| `rate_limit_per_sec` | 按需要设（0=不限） |
| `ledger_path` | `usage.db`（空=关闭账本） |

## 5. 升级流程

```bash
git pull origin main
cargo build --release
# 备份 usage.db（若有）
kill <网关PID> && ./target/release/deepseek-es-2api --config config.json
```

## 6. 排障信息采集

```bash
RUST_LOG=debug ./target/release/deepseek-es-2api --config config.json 2>&1 | tee gw.log
# 关键日志串：
#   "开始 Turnstile 认证流程" / "安全 cookie 兑换成功"
#   "上游要求重新安全校验，强制重认证后重试"
#   "上游首事件即配额耗尽，返回 429"
#   "cookie 临近过期，后台预取续期（M8）"
#   "上游未识别事件（已忽略）"  ← 上游协议可能变了
```

## 7. 回滚

```bash
git checkout <上一个 tag>
cargo build --release
# 重启
```

---

# ADR · 架构决策记录

## ADR-001 · 用协议翻译而非透传实现工具调用
**背景**：上游 AIPKit 不支持原生 function calling。
**决策**：网关侧「注入工具说明 → 解析 ```tool 块 → 产出标准 tool_calls/tool_use」。
**理由**：客户端能收到合法协议结构并回传结果，真实模型基于结果作答（E2E 证明）。非伪造。
**代价**：依赖模型遵循格式；不遵循时回退纯文本。

## ADR-002 · 字节级 SSE 缓冲（而非逐 chunk 字符串解码）
**背景**：TCP chunk 边界任意，可能切断 UTF-8 码点。
**决策**：`Vec<u8>` 缓冲 + 完整事件边界才解码。
**理由**：逐 chunk `from_utf8_lossy` 会静默损坏 CJK/emoji（H1）。
**代价**：内存稍增（有 8MB 上限保护）。

## ADR-003 · 双 HTTP 客户端（AJAX + SSE）
**背景**：reqwest `.timeout()` 覆盖整个 body 读取，会截断长流。
**决策**：AJAX 客户端带总超时；SSE 客户端只带 connect + read-idle 超时。
**理由**：长回答不应被固定总超时截断（M2）。

## ADR-004 · 网关侧多轮历史渲染（而非依赖上游 conv_uuid）
**背景**：标准客户端每轮发完整历史；多数 SDK 不设 `user` 字段 → 上游会话映射不稳定。
**决策**：多轮请求把完整历史渲染进 prompt；单轮保持原样。
**理由**：避免历史被静默丢弃（H2）；网关无状态、每轮独立可复现。

## ADR-005 · 只暴露单一诚实模型
**背景**：本站只有 1 个 bot，provider 固定 DeepSeek，代理无法切换。
**决策**：只暴露 `deepseek-es`；历史别名保留兼容但不列为独立模型。
**理由**：诚实优先，避免伪造「多 provider 路由」。

## ADR-006 · 上游未启用的能力不做（如实标注）
**背景**：图片/向量库/OpenAI 响应链/联网等上游 `allowXxx=false`。
**决策**：不实现，标「不适用」并说明原因。
**理由**：转发参数上游不响应，实现即伪造。判断标准 = 能否产出客户端可用的真实结果。
