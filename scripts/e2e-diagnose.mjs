#!/usr/bin/env node
/**
 * 诊断：为什么流式回复 0 chunks？打印网关返回的原始 SSE 字节。
 * 用法: node scripts/e2e-diagnose.mjs
 */
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..');
const BIN = path.join(ROOT, 'target', 'release', process.platform === 'win32' ? 'deepseek-es-2api.exe' : 'deepseek-es-2api');
const PORT = 47834;
const BASE = `http://127.0.0.1:${PORT}`;

const cfgPath = path.join(ROOT, 'target', 'e2e-diag-config.json');
fs.writeFileSync(cfgPath, JSON.stringify({
  listen_addr: `127.0.0.1:${PORT}`,
  upstream_base_url: 'https://deepseek.es',
  bot_id: '27623',
  sitekey: '0x4AAAAAADlLZ3ljqZP6cQwq',
  cf_solver_url: 'http://127.0.0.1:8001',
  user_agent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36',
  default_model: 'deepseek-es',
  api_keys: [],
  proxy: process.env.UPSTREAM_PROXY || 'http://127.0.0.1:10808',
  solver_timeout_secs: 120,
  http_timeout_secs: 300,
  cookie_ttl_secs: 1800,
  cors_allow_origins: [],
}, null, 2));

const child = spawn(BIN, ['--config', cfgPath], { env: { ...process.env, RUST_LOG: 'debug' }, stdio: ['ignore', 'pipe', 'pipe'] });
const logs = [];
child.stdout.on('data', d => { logs.push(d.toString()); process.stdout.write('[gw] ' + d); });
child.stderr.on('data', d => { logs.push(d.toString()); process.stdout.write('[gw!] ' + d); });
process.on('exit', () => { try { child.kill(); } catch {} });

async function waitHealth() {
  for (let i = 0; i < 50; i++) {
    try { const r = await fetch(`${BASE}/healthz`); if (r.ok) return; } catch {}
    await new Promise(s => setTimeout(s, 300));
  }
  throw new Error('网关未就绪');
}

async function main() {
  await waitHealth();
  console.log('[diag] 网关就绪');

  // 直接用流式，打印原始字节
  const r = await fetch(`${BASE}/v1/chat/completions`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model: 'deepseek-es', messages: [{ role: 'user', content: '请用中文回答：1+1等于几' }], stream: true }),
  });
  console.log('[diag] HTTP', r.status);
  const text = await r.text();
  console.log('[diag] === 原始 SSE 响应 ===');
  console.log(text.slice(0, 4000));
  console.log('[diag] === 响应长度:', text.length, '===');

  fs.writeFileSync(path.join(ROOT, 'target', 'e2e-diag-result.txt'), text);
}

main().catch(e => { console.error('❌', e.message); process.exit(1); });
