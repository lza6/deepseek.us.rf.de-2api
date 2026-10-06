#!/usr/bin/env node
/**
 * 真实自愈 E2E。
 *
 * 目标：用「真实上游 + 真实 Turnstile token」验证网关在收到上游 ts_required 后
 *       能自动重新认证并重试成功。
 *
 * 保真设计（哪些是真实的）：
 *   [真实] Turnstile token：由真实 cf_solver 预先对 https://deepseek.es 求解得到，
 *          写入 tokens 文件，本脚本的 solver 桩按序返回（token 本身 100% 真实，
 *          origin/action 均正确）。
 *   [真实] deepseek_ts_verify / nonce / cache_sse_message / SSE 正文：全部经 shim
 *          真实转发到 https://deepseek.es（走出口代理），并回传真实 set-cookie。
 *   [真实] 注入的 ts_required 响应：内容取自真实上游在无 cookie 时的真实抓包。
 *   [合成] 仅一点：**第一个** SSE 响应被替换为上条真实 ts_required，用于触发自愈；
 *          自愈后的重试走真实上游，得到真实 AI 回复。
 *
 * 前置：
 *   1. cf_solver 运行中（用于预取 token）
 *   2. 出口代理可用（默认 http://127.0.0.1:10808）
 *   3. tokens 文件（默认 /tmp/tokens.txt，每行一个真实 token）
 *
 * 用法: node scripts/e2e-selfheal.mjs [问题]
 */
import http from 'node:http';
import { spawn, execFile } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';

const ROOT = path.resolve(import.meta.dirname, '..');
const BIN = path.join(ROOT, 'target', 'release', process.platform === 'win32' ? 'deepseek-es-2api.exe' : 'deepseek-es-2api');
const SHIM_PORT = 47910;   // 网关→此处→真实上游
const STUB_PORT = 47912;   // 求解器桩（返回预取真实 token）
const GW_PORT = 47911;
const BASE = `http://127.0.0.1:${GW_PORT}`;
const REAL_UPSTREAM = 'https://deepseek.es';
const PROXY = process.env.UPSTREAM_PROXY || 'http://127.0.0.1:10808';
const TOKENS_FILE = process.env.TOKENS_FILE || '/tmp/tokens.txt';

// 让 Node fetch(undici) 走出口代理
process.env.NODE_USE_ENV_PROXY = '1';
process.env.HTTPS_PROXY ||= PROXY;
process.env.HTTP_PROXY ||= PROXY;

// 真实上游在无有效 dsts_ok cookie 时返回的首事件（已抓包确认，HTTP 200）
const REAL_TS_REQUIRED =
  'event: error\nid: err-1791266046\ndata: {"error":"Sicherheitspruefung erforderlich.","ts_required":true}\n\nevent: done\ndata: {"finished":true}\n\n';

function log(...a) { console.log('[selfheal]', ...a); }

// ── 求解器桩：返回预取的真实 token（每调用一次消耗一个）──────────
function startSolverStub(tokens) {
  let idx = 0;
  const server = http.createServer((req, res) => {
    const u = new URL(req.url, 'http://x');
    if (u.pathname === '/turnstile') {
      const token = tokens[idx] || tokens[tokens.length - 1];
      idx++;
      res.writeHead(202, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ task_id: `stub-${idx}`, status: 'accepted' }));
    } else if (u.pathname === '/result') {
      const token = tokens[(idx - 1) >= 0 ? idx - 1 : 0];
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ status: 'success', value: token }));
    } else {
      res.writeHead(404); res.end('{}');
    }
  });
  return new Promise((r) => server.listen(STUB_PORT, '127.0.0.1', () => r(server)));
}

// ── 上游 shim：转发到真实 deepseek.es；首个 SSE 注入真实 ts_required ──
let sseSeen = 0;
function startShim() {
  const server = http.createServer(async (req, res) => {
    const isSSE = req.url.includes('action=aipkit_frontend_chat_stream');
    if (isSSE) {
      sseSeen++;
      // 对照实验：SKIP_INJECT=1 时首次也转发真实上游（不注入 → 不触发自愈）
      if (sseSeen === 1 && !process.env.SKIP_INJECT) {
        log('shim: 首次 SSE → 注入真实上游 ts_required（触发自愈）');
        res.writeHead(200, { 'content-type': 'text/event-stream' });
        res.end(REAL_TS_REQUIRED);
        return;
      }
      log(`shim: 第 ${sseSeen} 次 SSE → 真实转发`);
      const r = await curlForward(req.method, req.url, req.headers, Buffer.alloc(0));
      const text = r.body.toString();
      const deltas = (text.match(/"delta":"[^"]*"/g) || []);
      const evs = (text.match(/^event: .*/gm) || []);
      log(`shim: 第 ${sseSeen} 次 SSE 上游 ${r.body.length}B | 事件=[${evs.join(',')}] | delta数=${deltas.length}`);
      // 剔除传输层头，避免双重 chunked 导致下游解析错乱
      const sseHeaders = { ...r.headers };
      delete sseHeaders['transfer-encoding'];
      delete sseHeaders['content-length'];
      delete sseHeaders['connection'];
      log(`shim: 发给网关的头: ${JSON.stringify(sseHeaders)}`);
      res.writeHead(r.status, sseHeaders);
      res.end(r.body);
      return;
    }
    const chunks = [];
    req.on('data', (c) => chunks.push(c));
    await new Promise((r) => req.on('end', r));
    const body = Buffer.concat(chunks);

    // 用 curl 子进程转发（curl 的 TLS 指纹能过 Cloudflare；Node 原生 https 会被 403）
    try {
      const r = await curlForward(req.method, req.url, req.headers, body);
      const outHeaders = { ...r.headers };
      delete outHeaders['transfer-encoding'];
      delete outHeaders['content-length'];
      delete outHeaders['connection'];
      if (r.setCookies.length) outHeaders['set-cookie'] = r.setCookies;
      res.writeHead(r.status, outHeaders);
      if (req.url.includes('admin-ajax.php')) {
        const action = (body.toString().match(/action=([a-z_]+)/) || [])[1] || '?';
        log(`shim: ${req.method} action=${action} → ${r.status} body=${r.body.toString().slice(0, 120)}`);
      }
      res.end(r.body);
    } catch (e) {
      log('shim 转发失败:', e.message);
      try { res.writeHead(502); res.end('shim err'); } catch {}
    }
  });
  return new Promise((r) => server.listen(SHIM_PORT, '127.0.0.1', () => r(server)));
}

// 用 curl 子进程转发到真实上游（经出口代理）
function curlForward(method, urlPath, headers, body) {
  return new Promise((resolve, reject) => {
    const hdrFile = path.join(os.tmpdir(), `sh_${process.pid}_${Date.now()}_${Math.random().toString(36).slice(2)}.hdr`);
    const args = ['-s', '--ssl-no-revoke', '-x', PROXY, '-X', method,
      '-H', 'Origin: ' + REAL_UPSTREAM, '-H', 'Referer: ' + REAL_UPSTREAM + '/', '-D', hdrFile];
    const h = headers;
    if (h['user-agent']) args.push('-A', h['user-agent']);
    if (h['cookie']) args.push('-H', 'Cookie: ' + h['cookie']);
    if (h['accept-language']) args.push('-H', 'Accept-Language: ' + h['accept-language']);
    if (h['accept']) args.push('-H', 'Accept: ' + h['accept']);
    if (h['content-type']) args.push('-H', 'Content-Type: ' + h['content-type']);
    if (body && body.length) args.push('--data-binary', '@-');
    args.push(REAL_UPSTREAM + urlPath);
    const child = spawn('curl', args, { stdio: ['pipe', 'pipe', 'pipe'] });
    const out = [];
    child.stdout.on('data', (d) => out.push(d));
    child.on('error', reject);
    child.on('close', () => {
      let hdr = '';
      try { hdr = fs.readFileSync(hdrFile, 'utf8'); fs.unlinkSync(hdrFile); } catch {}
      let status = 200;
      const outHeaders = {};
      const setCookies = [];
      for (const ln of hdr.split(/\r?\n/)) {
        const m = ln.match(/^HTTP\/[\d.]+ (\d+)/);
        if (m) { status = parseInt(m[1], 10); continue; }
        const ci = ln.indexOf(':');
        if (ci > 0) {
          const k = ln.slice(0, ci).trim().toLowerCase();
          const v = ln.slice(ci + 1).trim();
          if (k === 'set-cookie') setCookies.push(v);
          else if (k) outHeaders[k] = v;
        }
      }
      resolve({ status, headers: outHeaders, setCookies, body: Buffer.concat(out) });
    });
    child.stdin.end(body && body.length ? body : undefined);
  });
}

async function waitHealth(timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try { const r = await fetch(`${BASE}/healthz`); if (r.ok) return await r.json(); } catch {}
    await new Promise((s) => setTimeout(s, 300));
  }
  throw new Error('网关未就绪');
}

// 从真实 cf_solver 预取 N 个真实 Turnstile token（对真实 deepseek.es 求解）
async function prefetchTokens(n, solverUrl = 'http://127.0.0.1:8001') {
  const tokens = [];
  for (let attempt = 1; attempt <= 12 && tokens.length < n; attempt++) {
    let taskId;
    try {
      const r = await fetch(`${solverUrl}/turnstile?url=${encodeURIComponent(REAL_UPSTREAM + '/')}&sitekey=0x4AAAAAADlLZ3ljqZP6cQwq&action=chat`);
      taskId = (await r.json()).task_id;
    } catch { await new Promise((s) => setTimeout(s, 3000)); continue; }
    for (let i = 0; i < 50; i++) {
      let j;
      try { j = await (await fetch(`${solverUrl}/result?id=${taskId}`)).json(); } catch { await new Promise((s) => setTimeout(s, 2000)); continue; }
      if (j.status === 'success' && j.value) { tokens.push(j.value); log(`  预取 token#${tokens.length} (${j.value.length} 字符)`); break; }
      if (j.status === 'error') { log(`  [尝试${attempt}] 求解失败，重试`); break; }
      await new Promise((s) => setTimeout(s, 2000));
    }
    await new Promise((s) => setTimeout(s, 2000));
  }
  return tokens;
}

async function main() {
  if (!fs.existsSync(BIN)) { console.error('未找到二进制，请先 cargo build --release'); process.exit(1); }

  let tokens;
  if (fs.existsSync(TOKENS_FILE)) {
    tokens = fs.readFileSync(TOKENS_FILE, 'utf8').split('\n').map((s) => s.trim()).filter(Boolean);
    log(`从 ${TOKENS_FILE} 载入 ${tokens.length} 个 token`);
  } else {
    log('未提供 token 文件，自动从真实 cf_solver 预取 2 个 token...');
    tokens = await prefetchTokens(2);
  }
  if (tokens.length < 2) { console.error(`token 不足（${tokens.length}），需 ≥2 个（首次认证 + 自愈重认证）`); process.exit(1); }

  const stub = await startSolverStub(tokens);
  const shim = await startShim();
  log(`shim 127.0.0.1:${SHIM_PORT} → ${REAL_UPSTREAM}（经代理）; solver 桩 127.0.0.1:${STUB_PORT}`);

  const cfgPath = path.join(ROOT, 'target', 'e2e-selfheal-config.json');
  fs.writeFileSync(cfgPath, JSON.stringify({
    listen_addr: `127.0.0.1:${GW_PORT}`,
    upstream_base_url: `http://127.0.0.1:${SHIM_PORT}`,  // 网关→shim→真实上游
    bot_id: '27623',
    sitekey: '0x4AAAAAADlLZ3ljqZP6cQwq',
    cf_solver_url: `http://127.0.0.1:${STUB_PORT}`,       // 求解器桩（返回真实 token）
    user_agent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36',
    default_model: 'deepseek-es',
    api_keys: [],
    proxy: null,                                          // 网关直连 shim；shim 自身走代理
    solver_timeout_secs: 120,
    http_timeout_secs: 120,
    cookie_ttl_secs: 1800,
    pseudo_chunk_chars: 0,
    cors_allow_origins: [],
  }, null, 2));

  const child = spawn(BIN, ['--config', cfgPath], {
    env: { ...process.env, RUST_LOG: 'info' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let gwLog = '';
  child.stdout.on('data', (d) => { gwLog += d; process.stdout.write('[gw] ' + d); });
  child.stderr.on('data', (d) => { gwLog += d; process.stdout.write('[gw!] ' + d); });
  const cleanup = () => { try { child.kill(); } catch {} try { shim.close(); } catch {} try { stub.close(); } catch {} };
  process.on('exit', cleanup);

  try {
    await waitHealth();
    log('网关就绪');
    const prompt = process.argv[2] || 'Di hola en una frase corta';
    log('发送 OpenAI 非流式请求（首帧将被 shim 注入真实 ts_required）...');

    const t0 = Date.now();
    const r = await fetch(`${BASE}/v1/chat/completions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model: 'deepseek-es', messages: [{ role: 'user', content: prompt }], stream: false }),
    });
    const body = await r.text();
    log(`HTTP ${r.status} (${Date.now() - t0}ms)`);
    log('响应:', body.slice(0, 300));

    const selfHealed = gwLog.includes('上游要求重新安全校验');
    const reauthed = gwLog.includes('安全 cookie 兑换成功');
    let content = '';
    try { content = JSON.parse(body).choices?.[0]?.message?.content || ''; } catch {}

    log('--- 断言 ---');
    log(`  触发自愈日志「上游要求重新安全校验」: ${selfHealed}`);
    log(`  重认证成功日志「安全 cookie 兑换成功」: ${reauthed}`);
    log(`  HTTP 200: ${r.status === 200}`);
    log(`  真实回复非空: ${content.length > 0} → ${JSON.stringify(content.slice(0, 80))}`);

    const pass = r.status === 200 && selfHealed && reauthed && content.length > 0;
    if (!pass) { console.error('❌ 真实自愈验证失败'); process.exitCode = 2; return; }
    log('✅ 真实自愈通过：ts_required → 重新认证 → 重试 → 真实回复');
  } finally {
    cleanup();
  }
}

main().catch((e) => { console.error('❌', e.message); process.exit(1); });
