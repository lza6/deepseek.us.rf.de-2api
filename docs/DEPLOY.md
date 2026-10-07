# 部署与运维

## 1. 前置条件

| 组件 | 版本 | 用途 |
|------|------|------|
| Rust | ≥ 1.89 | 编译网关 |
| Python | 3.11 | 运行 cf_solver（camoufox） |
| camoufox | latest | CF Turnstile 浏览器求解 |

## 2. 部署拓扑

推荐**同机部署**：网关 + cf_solver 在同一台机器，cf_solver 通过 `127.0.0.1:8001` 被网关调用。

```
[公网/内网客户端]
        │  HTTP (OpenAI/Anthropic)
        ▼
[deepseek-es-2api :47833]
        │  HTTP (本地)
        ▼
[cf_solver :8001]  ──(camoufox 浏览器)──▶ challenges.cloudflare.com
        │
        │  HTTPS (可经代理)
        ▼
[deepseek.es]
```

## 3. 启动顺序

```bash
# 1) cf_solver（先启动，等待浏览器池就绪）
cd tools/cf_solver
python boterdrop_wrapper.py &          # 等待日志 "Pool siap: N halaman"（约 90s）

# 2) 网关
cp config.example.json config.json
export PROXY=http://127.0.0.1:10808    # 若需出口代理
./target/release/deepseek-es-2api --config config.json
```

## 4. 生产注意事项

### 4.1 鉴权（必须）
默认 `api_keys: []` = 仅本机放行。**暴露到内网/公网前必须配置**：
```bash
export API_KEYS="sk-your-strong-key-1,sk-your-strong-key-2"
```
或 `config.json` 的 `api_keys` 数组。

### 4.2 出口代理
若服务器直连 `deepseek.es` 受限（返回 302），需配置出口代理：
```json
{ "proxy": "http://127.0.0.1:10808" }
```

### 4.3 资源估算
| 资源 | cf_solver | 网关 |
|------|-----------|------|
| 内存 | ~600MB（camoufox 浏览器 × 页池） | ~30MB |
| CPU | 求解时瞬时高（~45-65s/次） | 低 |
| 磁盘 | ~400MB（浏览器） | 6.6MB |

### 4.4 并发与配额
- 网关侧：认证（Turnstile）**串行化**，避免重复求解；聊天请求可并发。
- 上游侧：deepseek.es 有配额限制（免费额度 + 代币），高并发会触发 `quota_notice`。
- **实测**：8 并发 × 24 请求 → 100% 成功（配额充足时）。
- 建议：生产环境加**下游限流**（如 tower-governor）与**请求队列**。

### 4.5 cf_solver 运维要点（实测）
- **进程生命周期**：约 55 分钟（后台任务被杀需重启）。
- **周期维护**：每 **10 分钟一次全量上下文重启**，期间约 **30–35s 不可用**；此时提交的求解任务可能失败。
- **单次求解耗时**：通常 45–65s；偶发抖动（实测有一次 31s 但含 87 次重试）。
- **应对**：求解失败时网关返回 502，客户端可重试；生产建议**多实例 cf_solver + 故障转移**（参考上层项目 `imagefree-2ai/api/solver_guard.py` 的熔断/负载均衡）。
- **验证就绪**：日志出现 `Pool siap: N halaman`。

## 5. 健康检查与监控

```bash
curl http://127.0.0.1:47833/healthz
# {"status":"ok","upstream":"https://deepseek.es","bot_id":"27623","cf_solver":"...","version":"0.9.0"}
```

日志（`RUST_LOG=info`）关键事件：
- `开始 Turnstile 认证流程` → `Turnstile 求解成功` → `安全 cookie 兑换成功`
- 认证失败 → 返回 502 + `error_type: api_error`

## 6. Docker

```bash
# 构建（仅网关；cf_solver 需单独部署，因含浏览器）
docker build -t deepseek-es-2api .
docker run -p 47833:47833 \
  -e CF_SOLVER_URL=http://host.docker.internal:8001 \
  -e API_KEYS=sk-strong \
  -e UPSTREAM_BASE_URL=https://deepseek.es \
  -v $(pwd)/config.json:/app/config.json \
  deepseek-es-2api
```

> Docker 版**不含 cf_solver**（浏览器依赖重）。生产建议两种方式：
> 1. 宿主机跑 cf_solver，容器经 `host.docker.internal:8001` 访问；
> 2. 自行构建 cf_solver 求解器镜像（本项目 `tools/cf_solver/` 仅含运行脚本，未提供 Dockerfile）。

## 7. 故障排查

| 症状 | 原因 | 处置 |
|------|------|------|
| 502 + `TsRequired` | Turnstile 求解失败/超时 | 检查 cf_solver；重试 |
| `400 invalid_request_error` | 请求体非法（空 messages） | 检查客户端请求 |
| 上游 `quota_notice` → 429 | 配额耗尽 | 等待额度恢复/购买 |
| 全部请求超时 | 出口网络受限 | 配置 `proxy` |
| cf_solver `Pool siap` 未出现 | 浏览器初始化失败 | 检查 camoufox 安装、内存 |
