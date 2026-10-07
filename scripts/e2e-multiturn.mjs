#!/usr/bin/env node
/**
 * 真实 E2E：多轮对话历史保留（H2 验证）。
 * 关键：客户端**不设** user/x-session-id（模拟标准 OpenAI SDK），
 * 第 2 轮必须仍能引用第 1 轮信息 —— 证明历史被完整送入上游。
 *
 * 用法: node scripts/e2e-multiturn.mjs
 */
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..');
const BIN = path.join(ROOT, 'target', 'release', process.platform === 'win32' ? 'deepseek-es-2api.exe' : 'deepseek-es-2api');
const PORT = 47835;
const BASE = `http://127.0.0.1:${PORT}`;

const cfgPath = path.join(ROOT, 'target', 'e2e-multiturn-config.json');
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
  cache_ttl_secs: 0, // 关闭缓存，避免干扰多轮验证
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

// 非流式调用（便于断言）
async function chat(messages) {
  const r = await fetch(`${BASE}/v1/chat/completions`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model: 'deepseek-es', messages, stream: false }),
  });
  const j = await r.json();
  return j.choices?.[0]?.message?.content ?? '';
}

async function main() {
  await waitHealth();
  console.log('[e2e] 网关就绪');

  // 第 1 轮：告知一个虚构事实（不设 user 字段，模拟标准 SDK）
  const r1 = await chat([{ role: 'user', content: '请记住：我最喜欢的颜色是紫色。只回复"好的"' }]);
  console.log('[e2e] 第1轮回复:', JSON.stringify(r1));

  // 第 2 轮：携带完整历史，问刚才说了什么
  const r2 = await chat([
    { role: 'user', content: '请记住：我最喜欢的颜色是紫色。只回复"好的"' },
    { role: 'assistant', content: r1 },
    { role: 'user', content: '我刚才说我最喜欢的颜色是什么？只回答颜色名' },
  ]);
  console.log('[e2e] 第2轮回复:', JSON.stringify(r2));

  const ok = r2.includes('紫');
  if (ok) {
    console.log('✅ H2 通过：多轮历史被完整保留（模型记得第1轮内容）');
  } else {
    console.log('❌ H2 失败：模型未记得第1轮内容，历史可能丢失');
    process.exit(2);
  }
}

main().catch(e => { console.error('❌', e.message); process.exit(1); });
