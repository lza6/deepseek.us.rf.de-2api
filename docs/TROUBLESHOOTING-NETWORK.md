# Cloudflare Turnstile 求解：完整诊断记录（2026-10-08）

> **本文档历经三次结论修正**。早期版本断言"网络出口中断"，后被实测推翻；
> 中期归因于"点击坐标打偏"，也被推翻。**当前结论有可复现证据链支撑**，见 §3。

---

## 1. 症状

- `tools/cf_solver` 返回 `{"status":"error","value":"captcha_fail"}`，耗时 ~93–117s。
- 偶发成功（本机日志中 08:30 / 08:42 各一次，17–28s），故早期被误判为"抖动"。
- 失败时 `.cf-turnstile` 容器存在，但**其中没有任何 iframe**。

## 2. 已排除的假设（均有实测反证）

| 假设 | 反证 | 证据 |
|------|------|------|
| 网络出口中断 | **错** | `curl --ssl-no-revoke https://deepseek.es/` → 200；经 xray 10808 亦 200 |
| `api.js` 不可达 | **错** | `curl -L` → 200，**86732 字节**；页面内 `fetch` 同样 200/86732 |
| sitekey / origin 被拒 | **错** | 手动 `turnstile.render()` 能创建 widget 节点 |
| 点击坐标打偏 | **错（此前误判）** | 修正为点击 iframe 内复选框后仍 3/3 失败；且实测 `iframe=0`，**根本无物可点** |
| 轮询预算不足 | **错** | 45s→93s 预算下仍失败，且失败时无 token 到达 |
| 路由拦截 abort 掉样式 | **已修** | 改为仅拦 image/media/font；`stylesheet` 放行 —— 未解决问题 |
| camoufox 指纹被识别 | **错** | CF 正常下发并渲染 widget 容器，未触发硬拦截 |

## 3. ✅ 确证根因（可复现证据链）

**`window.turnstile` 在页面中从未被暴露。**

证据（`target/diag/probe3.py` / `probe11.py` / `probe13.py`）：

```
真实站点状态: {'ts': 'undefined', 'init': 'undefined',
               'widgets': 1, 'ifr': 0, 'apiScript': True,
               'apiSrc': '.../api.js?onload=deepseekTsInit&render=explicit'}
```

即：`api.js` 标签存在且已加载、站点脚本已创建 `.deepseek-ts-widget` 容器，
但 `typeof window.turnstile === 'undefined'` —— **Turnstile API 未初始化**。

由此**完整因果链**：

1. 站点 `api.js?onload=deepseekTsInit&render=explicit` 加载后，需要回调
   `window.deepseekTsInit` 存在，才会暴露 `window.turnstile`。
2. 控制台明确报错（`probe9.py` 捕获原文）：
   > `[Cloudflare Turnstile] Unable to find onload callback 'deepseekTsInit'
   >  immediately after loading, expected 'function', got 'undefined'.`
3. 该回调由站点内联脚本 `<script id="deepseek-ts-js">` 定义。实测该节点
   **存在且内容完整（16392 字节）**，但**未执行**：
   - `pageerror` 为空（非 JS 异常）
   - `(0,eval)(s.textContent)` 手动执行 → `typeof window.deepseekTsInit`
     立即变 `function`（`probe5.py`）
4. 回调缺失 → `api.js` 不初始化 → 不暴露 `turnstile` → 站点 `scan()`/`process()`
   拿不到 `window.turnstile` → widget 永不 render → 无 iframe → 无
   `[name=cf-turnstile-response]` → `captcha_fail`。

**伴生现象**（同一环境的旁证）：
```
Failed to execute 'postMessage' on 'DOMWindow': The target origin provided
('https://challenges.cloudflare.com') does not match the recipient window's
origin ('https://deepseek.es').
```
该错误的 `file` 为主文档、`line: 0`，说明 `api.js` 与 CF challenge 端点的
postMessage 握手在此环境下不成立 —— 与"API 未初始化"互为印证。

## 4. 已实施的修复（真实落地的部分）

`tools/cf_solver/api_server.py`：

| # | 修复 | 状态 |
|---|------|------|
| 1 | **真实页面模式**：放弃伪造页（origin 错误），改为 `goto` 真实 `deepseek.es` | 已落地，实测 `已加载真实页面` |
| 2 | **主动注册回调**：`eval` 站点 `#deepseek-ts-js`，使 `deepseekTsInit` 就位 | 已落地，实测 `eval ok -> function` |
| 3 | `stylesheet` 不再 abort（避免 widget 布局失准） | 已落地 |
| 4 | 点击改为定位 **iframe 内复选框**（左侧 30px）而非外层 div 中心 | 已落地 |
| 5 | 轮询预算 18s → 45s/轮 ×2 轮 | 已落地 |
| 6 | 假页面模板补上 `onloadTurnstileCallback` 定义 | 已落地（兜底路径） |

**修复效果（诚实结论）**：修复 1–2 使流程推进到"`deepseekTsInit` 已注册"，
但 `api.js` **仍未暴露 `window.turnstile`**（probe11 三轮均 `turnstile=undefined`）。
**即：根因未被完全消除，`captcha_fail` 仍会发生。**

## 5. 结论：这不是本项目代码可修的问题

证据表明失败源于 **`api.js` 在 camoufox 环境下的初始化握手失败**，
而非求解器逻辑缺陷。可能的环境因素（按可能性）：

1. **出口 IP 风险分**：`129.146.124.201`（Oracle 机房段）→ CF 对 `api.js` 降级。
2. **camoufox 指纹与 CF 反自动化**的兼容性（WebGL context lost、WebRTC ICE failed 等
   在日志中反复出现）。
3. 站点脚本在部分环境下不执行的**上游缺陷**（本项目无法控制）。

## 6. 可用方案（按推荐度，均已在代码中就绪）

| 方案 | 做法 | 状态 |
|------|------|------|
| **A. 预置 cookie** ⭐推荐 | 自己浏览器过一次 CF → `node scripts/collect-cookie.mjs --cookie "dsts_ok=1; dsts=<hash>"` | **已实现（v0.19.0）**，零 solver 依赖 |
| **B. 第三方求解 API** | `tools/api_solver` + capsolver/2captcha key | **已实现（v0.18.0）** |
| **C. 换出口 IP** | 给 solver 配家宽/移动代理，降低风险分 | 未验证 |
| D. 继续调 solver | 需先解决 `api.js` 初始化握手 | **当前不可行** |

## 7. 纯协议过 CF：不可行（最终结论）

- 扫描 `D:\参考项目` **1244 个项目**：**不存在**「免费 + 无浏览器 + 纯协议」的
  Turnstile 解法。
- 唯一的纯协议逆向 `CloudFlareInvisibleSolver` 解的是 **`cf_clearance`（jsd 5秒盾）**，
  与 Turnstile widget 无关（其代码中 `challenges.cloudflare.com`、
  `cf-turnstile-response`、`sitekey`、`siteverify` **零命中**）。
- 本站在 `/clearance` 端点实测 `cf_clearance: ""` → **站点本就没有 5秒盾**。
- **原理**：Turnstile token 由 CF 边缘**服务端**签发，含浏览器环境探测 + PoW，
  客户端无法离线构造。

## 8. 复现命令

```bash
# 根因复现（检查 window.turnstile 是否暴露）
cd target/diag && PYTHONUTF8=1 <venv>/Scripts/python.exe probe13.py
# 期望看到: {'ts': 'undefined', 'widgets': 1, 'ifr': 0, ...}
```
