## CF 求解的**三层策略**（按可靠性排序）

Turnstile Managed Challenge 的 token **只能由真实浏览器生成**（完整调查见下）。因此本网关提供三层应对，
任一层可用即不中断：

| 层级 | 方式 | 配置 | 依赖 | 适用 |
|------|------|------|------|------|
| **① 预置 cookie（最可靠）** | 你在自己浏览器里过一次 CF，把 cookie 交给网关 | `initial_cookies` | 只需一次浏览器 | **推荐**：cf_solver 挂 / 网络不稳时用它 |
| **② 浏览器求解** | camoufox（`tools/cf_solver`） | `cf_solver_url` | 本机浏览器 + 网络 | 长期无人值守 |
| **③ 第三方 API** | capsolver/2captcha（`tools/api_solver`） | `solver_urls` 追加 | 付费 key + 能访问其 API | 无本机浏览器时 |

三者可**并存**：`solver_urls` 做轮询与故障转移；`initial_cookies` 命中时**完全不触发求解**。

### ① 预置 cookie（一键采集）

```bash
# 1. 你自己浏览器打开 https://deepseek.es/ ，等页面能正常发消息（已过 Turnstile）
# 2. F12 → Application → Cookies → 复制 dsts_ok 与 dsts 两行
# 3. 交给网关（自动写入 config.json 的 initial_cookies）
node scripts/collect-cookie.mjs --cookie "dsts_ok=1; dsts=<hash>"
#    或从导出的 cookie 文件读：
node scripts/collect-cookie.mjs --file cookies.txt
```

**有效期**：上游 `dsts` cookie 约 3.5 小时（见 `分析文档/06` §4.1），过期后重跑一次采集即可。

---

## 为什么不能"纯协议"过 Turnstile（技术边界，已彻查）

**结论：不存在免费 + 无浏览器 + 纯协议的 Turnstile 解法**（已扫描 `D:\参考项目` 全部 1244 个项目）。

| 候选方案 | 为什么不行 |
|---------|-----------|
| `CloudFlareInvisibleSolver`（纯协议逆向） | 解的是 **`cf_clearance`（jsd 5秒盾）**，**不是 Turnstile widget token** |
| `geetest-bypass` | 极验的，与 Cloudflare 无关 |
| `Cloudflare-Faker` | Java 服务 + Chrome 扩展，**必须 GUI 机器** |
| `captcha-solver` / `ohmycaptcha` | 仍需 CloakBrowser / Playwright 浏览器引擎 |
| `cf-turnstile-token`（Peak API） | 纯 HTTP，但**付费第三方** |
| `riskbypass_demo` | 契约最完整，但**付费第三方** |

**根因**：Turnstile token 是 Cloudflare 边缘在**服务端**签发的不透明串，其生成包含
**服务端不可见的浏览器环境探测 + PoW**。客户端只能"真实地"让它生成，无法离线构造 ——
除非持续逆向每次更新的 `api.js`（CF 频繁轮换），投入产出比极低。

上层项目的 `imagefree-2ai/api/cf_clearance_solver.py` 自己也在文档里写明：
「**不用于 Turnstile widget**（那是 cf_solver 浏览器求解）」。

### 已落地的改进（来自对参考项目的研究）

- **`initial_cookies` + `scripts/collect-cookie.mjs`** —— 用你自己的浏览器当求解器，**零依赖**（本版本新增）。
- **`tools/api_solver/`** —— 纯 HTTP 第三方 API 适配器（capsolver/2captcha），作为第三层（上一版本新增）。
- **待办（有价值）**：借鉴 `captcha-solver` 的 **`verify_url` 同会话提交**机制（解决假页面 token 被
  `invalid-input-response` 拒的问题）；借鉴 `riskbypass` 契约（返回配套 `ua`）。
# CF 求解故障：精确定位与处置（2026-10-08）

> 本文档由实际探测得出，**每一步都有命令与输出**，非推测。

## 一句话结论

**xray 代理的上游节点已失效** → 浏览器无法到达 Cloudflare → Turnstile 求解 `captcha_fail`。
**这是本机网络/代理配置问题，不是网关代码问题。**

## 探测证据链

### 1. cf_solver 能跑，但求解失败

```bash
curl -s "http://127.0.0.1:8001/turnstile?url=https%3A%2F%2Fdeepseek.es%2F&sitekey=0x4AAAAAADlLZ3ljqZP6cQwq&action=chat"
# → {"task_id":"b36a...","status":"accepted"}          ← 服务正常
curl -s "http://127.0.0.1:8001/result?id=b36a..."
# → {"status":"error","elapsed_time":75.877,"value":"captcha_fail"}   ← 求解失败（75s 后才失败）
```

### 2. 出口 TCP 全部不可达（直连）

```bash
timeout 5 bash -c 'echo > /dev/tcp/1.1.1.1/443'     # 不可达
timeout 5 bash -c 'echo > /dev/tcp/8.8.8.8/53'      # 不可达
```

### 3. 代理进程在跑，但对境外的域名全不通

```bash
tasklist | grep 4332       # → xray.exe
netstat -ano | grep LISTENING | grep 4332   # → 0.0.0.0:10808, 127.0.0.1:10812

curl -x http://127.0.0.1:10808 -o /dev/null -w "%{http_code}" https://1.1.1.1/           # → 301 ✅
curl -x http://127.0.0.1:10808 -o /dev/null -w "%{http_code}" https://www.cloudflare.com/  # → 000 ❌
curl -x http://127.0.0.1:10808 -o /dev/null -w "%{http_code}" https://deepseek.es/          # → 000 ❌ (0.695s 即断)
curl -x http://127.0.0.1:10808 -o /dev/null -w "%{http_code}" https://www.google.com/       # → 000 ❌
curl -x http://127.0.0.1:10808 -o /dev/null -w "%{http_code}" https://api.github.com/       # → 000 ❌
```

**解读**：
- `1.1.1.1`（Cloudflare 的公共 DNS）**在国内可直连**，所以它的 301 是**直连结果**，不是代理生效。
- 一切**需要走代理出境**的域名**全部 000**（含 cloudflare.com 自身）。
- `deepseek.es` 在 0.695s 内断开 —— **连接被拒**（非超时），典型的节点不可用表现。

### 4. 代理软件确实是 xray

```bash
tasklist /FI "PID eq 4332"
# → xray.exe        ← 代理内核
```

`0.0.0.0:10808` = 混合入站（HTTP+SOCKS）；`127.0.0.1:10812` = xray 的 API/其它入站。

## 处置（需你侧操作）

### 方案 A：更换/修复 xray 节点（**首选**）

1. 打开你的代理客户端（v2rayN / Nekoray / Clash 等，内核是 xray）。
2. **更新订阅**（节点可能已失效或被墙）。
3. 切换到**可用的节点**，然后用命令验证：
   ```bash
   curl -x http://127.0.0.1:10808 -o /dev/null -w "%{http_code}\n" https://www.cloudflare.com/
   # 期望：200 或 403（**不能是 000**）
   ```
4. 再验证目标站点：
   ```bash
   curl -x http://127.0.0.1:10808 -o /dev/null -w "%{http_code}\n" https://deepseek.es/
   # 期望：200/403（非 000）
   ```
5. 两项都非 000 后，重启 cf_solver 并重跑 E2E：
   ```bash
   cd tools/cf_solver
   "/c/Users/Administrator.DESKTOP-EGNE9ND/Desktop/2api目录/imagefree-2ai/.venv/Scripts/python.exe" boterdrop_wrapper.py
   # 等 8001 LISTENING
   node ../../scripts/e2e-v1.mjs
   ```

### 方案 B：改用第三方 captcha API（**已实现**，见 `tools/api_solver/`）

若网络长期不稳，可完全绕开"本机出口"——由**第三方服务**（其服务器在墙外）代为求解：

```bash
API_SOLVER_KEY=<你的 capsolver/2captcha key> node tools/api_solver/server.mjs   # → :8002
```
```jsonc
// config.json：camoufox 为主、API 为备
{ "solver_urls": ["http://127.0.0.1:8001", "http://127.0.0.1:8002"] }
```

> 注意：**本机仍需能访问第三方 API 的域名**（capsolver.com / 2captcha.com）。
> 若这些也走同一失效节点，方案 B 同样不通 —— 仍需先修节点（方案 A）。

### 方案 C：仅在直连可达时使用

若某段时间出口正常（`1.1.1.1` 与 `deepseek.es` 均通），则把 `proxy` 置为 `null` 直连：
```jsonc
{ "proxy": null }
```
实测本机**直连也不通**，故当前不可行；仅在未来网络环境变化时可用。

## 为什么不能"纯协议求解"（技术边界，勿再尝试）

deepseek.es 用的是 **Turnstile Managed Challenge**，三处证据：

1. 上游 JS 注释：`// Turnstile rendern (Managed)`
   （`抓包验证/deepseek-ts-security.js:99`）
2. 页面含 `challenges.cloudflare.com/turnstile` widget（`抓包验证/home.html`，10 处引用）
3. 上层项目 `imagefree-2ai/api/cf_clearance_solver.py` 文档明确：
   「仅用于非敏感的 CF 5s 盾穿越，**不用于 Turnstile widget**（那是 cf_solver 浏览器求解）」

**Managed Challenge 的 token 由 CF 服务端在验证 JS 执行 + 浏览器指纹后签发**，
其**设计目标就是不可纯协议伪造**。上行项目里那个"纯协议"文件解决的是
**cf_clearance（5 秒盾）**，与 Turnstile widget 是两回事。

可行路径**只有**：① 浏览器求解（camoufox / cf_solver）；② 第三方求解服务（方案 B，已实现）。
