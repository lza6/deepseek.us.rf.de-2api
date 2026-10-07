#!/usr/bin/env node
/**
 * 真实 E2E：v0.8.0 新增端点（连真实上游 deepseek.es + cf_solver）
 *   - GET  /v1/balance       上游配额/余额（网关身份）
 *   - GET  /v1/conversations 会话列表
 *   - DELETE /v1/conversations?id=  删除会话
 *   - M10：流式配额 → 429（需上游真的配额耗尽，通常无法触发；此处仅验证端点可达）
 *
 * 用法: node scripts/e2e-meta.mjs
 */
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..');
const BIN = path.join(ROOT, 'target', 'release', process.platform === 'win32' ? 'deepseek-es-2api.exe' : 'deepseek-es-2api');
const PORT = 47840;
const BASE = `http://127.0.0.1:${PORT}`;

const cfgPath = path.join(ROOT, 'target', 'e2e-meta-config.json');
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
  connect_timeout_secs: 20,
  cookie_ttl_secs: 1800,
  cors_allow_origins: [],
  cache_ttl_secs: 0,
}, null, 2));

const child = spawn(BIN, ['--config', cfgPath], { env: { ...process.env, RUST_LOG: 'info' }, stdio: ['ignore', 'pipe', 'pipe'] });
child.stdout.on('data', d => process.stdout.write('[gw] ' + d));
child.stderr.on('data', d => process.stdout.write('[gw!] ' + d));
process.on('exit', () => { try { child.kill(); } catch {} });

async function waitHealth() {
  for (let i = 0; i < 60; i++) {
    try { const r = await fetch(`${BASE}/healthz`); if (r.ok) return; } catch {}
    await new Promise(s => setTimeout(s, 300));
  }
  throw new Error('网关未就绪');
}

let pass = 0, fail = 0;
const check = (n, ok, d = '') => { if (ok) { pass++; console.log(`✅ ${n}`); } else { fail++; console.log(`❌ ${n} ${d}`); } };

async function main() {
  await waitHealth();
  console.log('[e2e] 网关就绪');

  // 先做一次聊天触发认证（余额/会话可能需 dsts cookie）
  const chat = await fetch(`${BASE}/v1/chat/completions`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model: 'deepseek-es', messages: [{ role: 'user', content: 'Di OK' }] }),
  });
  console.log('[e2e] 预热聊天:', chat.status);

  // ── /v1/balance ──
  const rb = await fetch(`${BASE}/v1/balance`);
  const bodyB = await rb.text();
  console.log('[e2e] /v1/balance:', rb.status, bodyB.slice(0, 200));
  check('/v1/balance 返回 200', rb.status === 200, `status=${rb.status}`);
  if (rb.status === 200) {
    try {
      const v = JSON.parse(bodyB);
      check('/v1/balance 含 balance 或 free 字段',
        v.balance !== undefined || v.free !== undefined, JSON.stringify(v).slice(0, 120));
    } catch (e) { check('/v1/balance 为 JSON', false, e.message); }
  }

  // ── /v1/conversations ──
  const rc = await fetch(`${BASE}/v1/conversations`);
  const bodyC = await rc.text();
  console.log('[e2e] /v1/conversations:', rc.status, bodyC.slice(0, 200));
  check('/v1/conversations 返回 200', rc.status === 200, `status=${rc.status}`);
  if (rc.status === 200) {
    try {
      const v = JSON.parse(bodyC);
      check('/v1/conversations 结构为 list', v.object === 'list' && Array.isArray(v.data), JSON.stringify(v).slice(0, 120));
    } catch (e) { check('/v1/conversations 为 JSON', false, e.message); }
  }

  // ── DELETE 缺 id → 400 ──
  const rd = await fetch(`${BASE}/v1/conversations`, { method: 'DELETE' });
  check('DELETE 缺 id → 400', rd.status === 400, `status=${rd.status}`);

  console.log(`\n===== E2E 结果: ${pass} 通过 / ${fail} 失败 =====`);
  if (fail > 0) process.exit(2);
}

main().catch(e => { console.error('❌', e.message); process.exit(1); });
