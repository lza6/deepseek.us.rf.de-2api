# 故障排查：cf_solver 无法启动 / E2E 全失败

> **记录时间**：2026-10-08
> **症状**：`cf_solver` 的 camoufox 报 `Failed to launch the browser process`；
> 网关 E2E 全部失败（`solver_failed` 502）。

## 根因（已实证，非猜测）

**机器网络出口完全中断**，逐层探测结果：

| 探测目标 | 结果 |
|---------|------|
| `1.1.1.1:443`（Cloudflare DNS） | **TCP 不可达** |
| `8.8.8.8:53`（Google DNS） | **TCP 不可达** |
| `104.21.19.181:443`（deepseek.es 的 CF IP） | **TCP 不可达** |
| 代理 `127.0.0.1:10808`（HTTP） | 端口在监听，但经其访问外网失败 |
| 代理 `127.0.0.1:10808`（SOCKS5） | 同样失败 |
| `nslookup deepseek.es` | **DNS 解析正常**（返回 CF IP） |

**结论**：DNS 正常但**所有 TCP 出站不通** → VPN/代理的**上游链路**断了（不是本机端口问题）。

## 影响链

```
网络出口断
  └→ camoufox 无法访问 challenges.cloudflare.com / Turnstile 脚本
      └→ cf_solver 浏览器启动失败 → 503
          └→ 网关 solver_failed → 502
              └→ 真实 E2E 全失败
```

**网关自身行为正确**：返回 `502 {"error":{"code":"solver_failed",...}}`（结构化错误体，v0.12.0 的 `error_code` 修复生效）。

## 处置

1. **恢复 VPN/代理出口连通**（本机侧操作，非代码问题）。
   - 验证：`curl -s -m 8 -x <proxy> -o /dev/null -w "%{http_code}" https://deepseek.es/` 应返回 200/403（而非 000）。
2. 网络恢复后启动 solver：
   ```bash
   cd tools/cf_solver
   "/c/Users/Administrator.DESKTOP-EGNE9ND/Desktop/2api目录/imagefree-2ai/.venv/Scripts/python.exe" boterdrop_wrapper.py
   # 等到日志出现 "Pool siap" / 8001 端口 LISTENING
   ```
   > 注意：本项目内 `python` 不在 PATH，**必须用上层项目的 venv python**。
3. 重跑 E2E：`node scripts/e2e-v1.mjs`、`node scripts/e2e-meta.mjs`。

## 为什么不能"纯协议"绕过 Turnstile（技术边界，如实说明）

上游是 **Turnstile Managed Challenge**（非 invisible），三处证据：

1. 上游 JS 注释：`// Turnstile rendern (Managed)`（`抓包验证/deepseek-ts-security.js`）
2. 页面含 `challenges.cloudflare.com/turnstile` widget（`抓包验证/home.html`，10 处引用）
3. 上层项目 `cf_clearance_solver.py` 的文档明确：**"仅用于非敏感的 CF 5s 盾穿越，
   **不用于 Turnstile widget**（那是 cf_solver 浏览器求解）"**

**Managed Challenge 的 token 由 CF 服务端在验证 JS 执行 + 浏览器指纹后签发**，
设计目标就是**不可纯协议伪造**。可行的替代只有：
- 浏览器求解（现状：camoufox，即 cf_solver）
- **第三方 captcha 求解服务**（capsolver / 2captcha 等提供 Turnstile 求解的 HTTP API）——
  若需要，可新增一个"solver 后端类型"接入（当前 `solver_urls` 只支持 camoufox 契约）。
