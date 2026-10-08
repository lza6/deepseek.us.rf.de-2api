#!/usr/bin/env node
/**
 * 从本地浏览器「采集」deepseek.es 的安全 cookie，交给网关 —— **完全绕过 solver**。
 *
 * ## 背景（为什么需要它）
 *
 * deepseek.es 用 Cloudflare **Turnstile Managed Challenge**。经完整调查（`docs/TROUBLESHOOTING-NETWORK.md`），
 * 其 token **只能由真实浏览器生成**，无法纯协议伪造。因此当 cf_solver
 * （camoufox 浏览器）或第三方 API 不可用时，**你机器上的浏览器**就是最可靠的求解器。
 *
 * 本脚本让你：
 *   1. 在自己的浏览器里打开 deepseek.es（正常通过 Turnstile）；
 *   2. 把浏览器里的 `dsts_ok` / `dsts` cookie 复制出来；
 *   3. 本脚本写入网关配置或直接调用网关的热加载接口。
 *
 * ## 用法
 *
 * ### 方式 A：手动粘贴（最简单，无需扩展）
 * 1. 浏览器打开 https://deepseek.es/ ，等页面底部出现「✓ Bestätigt」/ 能正常发消息；
 * 2. F12 → Application → Cookies → 复制 `dsts_ok` 与 `dsts` 两行；
 * 3. 运行：
 *    ```bash
 *    node scripts/collect-cookie.mjs --cookie "dsts_ok=1; dsts=<hash>"
 *    ```
 *    或从文件读：`--file cookies.txt`
 *
 * ### 方式 B：从浏览器导出的 cookie 文件读取
 * 用任意「cookie 导出」扩展导出 Netscape/JSON 格式，本脚本自动解析出所需字段。
 *
 * ## 输出
 *
 * - 更新 `config.json` 的 `initial_cookies`（若存在）；
 * - 若网关正在运行且启用了 `admin`，可 `--hot` 直接 POST 热更新（不重启）。
 *
 * ## 安全
 *
 * cookie 是敏感凭证，本脚本**不会**把 cookie 打印到日志（只打印长度与掩码），
 * 也**不会**写入任何非项目目录。
 */

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';

const ROOT = path.resolve(import.meta.dirname, '..');
const WANTED = ['dsts_ok', 'dsts'];
const CF_COOKIES = ['cf_clearance']; // 若站点同时有 CF 盾，一并带上

/** 从 `name=value; name2=value2` 串里挑出我们需要的 cookie。 */
export function pickCookies(raw) {
  const out = {};
  for (const seg of raw.split(';')) {
    const s = seg.trim();
    if (!s) continue;
    const i = s.indexOf('=');
    if (i <= 0) continue;
    const k = s.slice(0, i).trim();
    const v = s.slice(i + 1).trim();
    if ([...WANTED, ...CF_COOKIES].includes(k)) out[k] = v;
  }
  return out;
}

/** 从 Netscape cookies.txt 或 JSON 数组里解析。 */
export function parseCookieFile(text) {
  const t = text.trim();
  // JSON 形式：[{name,value},...] 或 {dsts:"..",dsts_ok:"1"}
  if (t.startsWith('[') || t.startsWith('{')) {
    try {
      const j = JSON.parse(t);
      const arr = Array.isArray(j) ? j : Object.entries(j).map(([name, value]) => ({ name, value }));
      const out = {};
      for (const c of arr) {
        const name = c.name ?? c.key;
        if ([...WANTED, ...CF_COOKIES].includes(name)) out[name] = String(c.value ?? c.val ?? '');
      }
      return out;
    } catch { /* 落到 Netscape 解析 */ }
  }
  // Netscape cookies.txt：每行 7 字段，第 6 是 name、第 7 是 value
  const out = {};
  for (const line of t.split('\n')) {
    if (!line || line.startsWith('#')) continue;
    const parts = line.split('\t');
    if (parts.length >= 7) {
      const name = parts[5];
      if ([...WANTED, ...CF_COOKIES].includes(name)) out[name] = parts[6];
    }
  }
  return out;
}

/** 掩码显示（不泄漏完整值）。 */
export function mask(v) {
  if (!v) return '(空)';
  if (v.length <= 6) return `***(len=${v.length})`;
  return `${v.slice(0, 4)}…${v.slice(-2)}(len=${v.length})`;
}

/** 生成 cookie 头串。 */
export function toCookieHeader(obj) {
  return Object.entries(obj).map(([k, v]) => `${k}=${v}`).join('; ');
}

// ── 主流程 ─────────────────────────────────────────────

function parseArgs(argv) {
  const a = { cookie: null, file: null, hot: false, adminBase: 'http://127.0.0.1:47833', token: null };
  for (let i = 2; i < argv.length; i++) {
    const k = argv[i];
    if (k === '--cookie') a.cookie = argv[++i];
    else if (k === '--file') a.file = argv[++i];
    else if (k === '--hot') a.hot = true;
    else if (k === '--admin') a.adminBase = argv[++i];
    else if (k === '--token') a.token = argv[++i];
  }
  return a;
}

function main() {
  const args = parseArgs(process.argv);
  if (!args.cookie && !args.file) {
    console.error('用法: node scripts/collect-cookie.mjs --cookie "dsts_ok=1; dsts=<hash>"');
    console.error('   或: node scripts/collect-cookie.mjs --file cookies.txt [--hot --token <admin_token>]');
    process.exit(1);
  }

  const found = args.cookie ? pickCookies(args.cookie) : parseCookieFile(fs.readFileSync(args.file, 'utf8'));

  if (!found.dsts_ok) {
    console.error('❌ 未找到 dsts_ok —— 请确认已在浏览器里通过 Turnstile（页面能正常发消息）后再导出。');
    process.exit(2);
  }
  if (!found.dsts) {
    console.warn('⚠️  未找到 dsts（服务端校验凭证）—— 仅 dsts_ok 可能不被上游接受，建议一并导出。');
  }

  const header = toCookieHeader(found);
  console.log('采集到的 cookie（已掩码）：');
  for (const [k, v] of Object.entries(found)) console.log(`  ${k} = ${mask(v)}`);

  // 写入 config.json（若存在）
  const cfgPath = path.join(ROOT, 'config.json');
  if (fs.existsSync(cfgPath)) {
    const cfg = JSON.parse(fs.readFileSync(cfgPath, 'utf8'));
    cfg.initial_cookies = header;
    fs.writeFileSync(cfgPath, JSON.stringify(cfg, null, 2) + '\n');
    console.log(`✅ 已写入 ${path.relative(ROOT, cfgPath)} 的 initial_cookies（重启网关生效）`);
  } else {
    console.log('提示：未找到 config.json，可将下面这行加入配置：');
    console.log(`  "initial_cookies": ${JSON.stringify(header)}`);
  }

  if (args.hot) {
    console.log(`提示：网关重启后生效。热更新需网关暴露 admin 接口（当前未实现该写入端点）。`);
  }
}

// 仅直接执行时跑主流程（被 import 时只导出纯函数，便于测试）
if (import.meta.url === `file://${process.argv[1]?.replace(/\\/g, '/')}`) {
  main();
}
