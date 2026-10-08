# CF Turnstile 求解失败 — 根因与修复方案

> **诊断日期**：2026-10-08 · **全部结论附实测证据**

## 一、根因（已实测确认）

### 证据 1：失败截图显示 CF 给出**交互式挑战**且 checkbox 未被勾选

`tools/cf_solver/debug_logs/20261008_135213_69ad1ef8_r2.png` 显示：
- Turnstile 渲染为 **「Verify you are human」+ 复选框**（交互式，非 invisible）
- **复选框是空白的**（未被点击）
- 页面 title = `CF Turnstile Solver`（**假页面**的 title，非 deepseek.es）

失败 JSON（同目录 `.json`）：
```json
{"turnstile_widget_exists": true, "cf_response_value": "", "js_errors": []}
```
→ widget 挂上了，但**永远拿不到 token**。

### 证据 2：solver 点击的是**错误元素**

`tools/cf_solver/api_server.py:463`：
```python
await page.locator("//div[@class='cf-turnstile']").click(timeout=100)
```
Turnstile 的复选框位于 **`challenges.cloudflare.com` 的跨域 iframe 内**。
点击外层 `div.cf-turnstile` **无法命中 iframe 内的 checkbox** → 永远点不到。

### 证据 3：solver 用**空白假页面**，被 CF 判高风险

`api_server.py:30-44` 的 `HTML_TEMPLATE` 是一个**近乎空白**的页面（只有 70×65 的 widget 容器），
通过 `page.route()` 替换目标 URL 的响应。CF 对"无实质内容的页面"会**升级为交互式挑战**。

### 证据 4：站点**真实页面**就能正常访问，且**内嵌了 widget**

```bash
curl -sG "http://127.0.0.1:8001/clearance" --data-urlencode "url=https://deepseek.es/"
# → {"status":"success","elapsed_time":13.023,"cf_clearance":"",
#    "cookies":"pll_language=es; dsgt_gid=..."}
```
- `cf_clearance` 为空 → **站点没有 CF 5 秒盾**（CF 只在 Turnstile widget 上）。
- 但**真实页面访问成功** → camoufox 能正常访问站点。

`抓包验证/home.html` 显示真实页面**内嵌了 Turnstile widget**（`deepseek-ts-widget` 5 处）。

## 二、修复方案（按优先级）

### 方案 A：改用**真实页面**求解（**推荐**）

不再用空白假页面，而是**访问真实页面**，让页面自己的 JS 渲染并求解 Turnstile，
然后从页面读出 token。

- **依据**：真实页面已内嵌 widget（证据 4），且 camoufox 能正常访问。
- **参考**：`D:\参考项目\captcha-solver` 的 **"Real Page"** 模式（调查确认可行）。
- **实现要点**：
  1. `page.goto(真实 URL)` 而非 route 替换；
  2. 等待 widget 自动求解（invisible/managed 通常自动过；若出现 checkbox 则需要点击）；
  3. **点击必须穿透 iframe**：用 `page.frame_locator("iframe[src*=challenges.cloudflare.com]")`
     定位后再点 checkbox（或 `page.mouse.click(x,y)` 点击 iframe 内坐标）；
  4. 从 `input[name=cf-turnstile-response]` 读 token。

### 方案 B：修正点击逻辑（配合 A）

```python
# 错误（当前）：
await page.locator("//div[@class='cf-turnstile']").click(timeout=100)
# 正确：穿透 CF 的 iframe
try:
    fl = page.frame_locator("iframe[src*='challenges.cloudflare.com']")
    await fl.locator("input[type=checkbox], label").click(timeout=1000)
except Exception:
    pass
```

### 方案 C：给 solver 配代理（换 IP，降低风险判定）

solver 的 `/turnstile` **支持 `proxy` 参数**，但 `_create_context_with_proxy` 实测**会卡死**
（>70s 无日志）。需先修该函数（加超时）。

### 方案 D：预置 cookie（**已实现**，v0.19.0）

若上述都不可行，`initial_cookies` + `scripts/collect-cookie.mjs` 用你自己的浏览器绕过。
（本次诊断已用真实 cookie 实测：绕过机制生效。）

## 三、为什么"纯协议"不可行（最终结论）

**站点用 Turnstile Managed Challenge**（证据：上游 JS 注释 `// Turnstile rendern (Managed)`）。
token 由 CF **边缘服务端**签发，生成过程含浏览器环境探测 + PoW，
**客户端无法离线构造**。唯一的"纯协议"参考项目 `CloudFlareInvisibleSolver` 解的是
**`cf_clearance`**（本站在证据 4 中已确认**没有**），**与 Turnstile 无关**。

**可行路径只有**：真实浏览器（方案 A/B/C）或第三方求解 API（`tools/api_solver`）。
