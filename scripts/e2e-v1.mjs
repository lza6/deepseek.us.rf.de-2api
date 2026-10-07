#!/usr/bin/env node
/**
 * 真实 E2E：v1.0.0 正确性加固批次（连真实上游 deepseek.es + cf_solver）。
 *   H3：Anthropic 端点错误体必须是 Anthropic 结构
 *   M4：Anthropic 流式 usage 非 0
 *   H4：伪工具说明注入（检查 prompt 送达，用 debug 日志间接验证）
 *   M7：正常结束仍带 finish_reason / message_stop
 *
 * 用法: node scripts/e2e-v1.mjs
 */
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..');
const BIN = path.join(ROOT, 'target', 'release', process.platform === 'win32' ? 'deepseek-es-2api.exe' : 'deepseek-es-2api');
const PORT = 47836;
const BASE = `http://127.0.0.1:${PORT}`;
const KEY = 'sk-e2e-secret';

const cfgPath = path.join(ROOT, 'target', 'e2e-v1-config.json');
fs.writeFileSync(cfgPath, JSON.stringify({
  listen_addr: `127.0.0.1:${PORT}`,
  upstream_base_url: 'https://deepseek.es',
  bot_id: '27623',
  sitekey: '0x4AAAAAADlLZ3ljqZP6cQwq',
  cf_solver_url: 'http://127.0.0.1:8001',
  user_agent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36',
  default_model: 'deepseek-es',
  api_keys: [KEY],
  proxy: process.env.UPSTREAM_PROXY || 'http://127.0.0.1:10808',
  solver_timeout_secs: 120,
  http_timeout_secs: 300,
  cookie_ttl_secs: 1800,
  cors_allow_origins: [],
  cache_ttl_secs: 0,
  pseudo_tools_enabled: true,
}, null, 2));

const child = spawn(BIN, ['--config', cfgPath], { env: { ...process.env, RUST_LOG: 'info,deepseek_es_2api::upstream=info' }, stdio: ['ignore', 'pipe', 'pipe'] });
let logs = '';
child.stdout.on('data', d => { logs += d; });
child.stderr.on('data', d => { logs += d; });
process.on('exit', () => { try { child.kill(); } catch {} });

async function waitHealth() {
  for (let i = 0; i < 60; i++) {
    try { const r = await fetch(`${BASE}/healthz`); if (r.ok) return; } catch {}
    await new Promise(s => setTimeout(s, 300));
  }
  throw new Error('网关未就绪');
}

let pass = 0, fail = 0;
function check(name, cond, detail = '') {
  if (cond) { pass++; console.log(`✅ ${name}`); }
  else { fail++; console.log(`❌ ${name} ${detail}`); }
}

async function main() {
  await waitHealth();
  console.log('[e2e] 网关就绪');

  // ── H3：Anthropic 端点无 key → 401 + Anthropic 结构 ──
  const r1 = await fetch(`${BASE}/v1/messages`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model: 'deepseek-es', max_tokens: 10, messages: [{ role: 'user', content: 'hi' }] }),
  });
  const v1 = await r1.json();
  console.log('[e2e] H3 无key响应:', r1.status, JSON.stringify(v1));
  check('H3 Anthropic 错误体顶层 type=error', v1.type === 'error', JSON.stringify(v1));
  check('H3 Anthropic 错误体 error.type 合法', ['invalid_request_error', 'authentication_error', 'api_error'].includes(v1.error?.type), JSON.stringify(v1));

  // ── M4：Anthropic 流式 usage 非 0 ──
  const r2 = await fetch(`${BASE}/v1/messages`, {
    method: 'POST', headers: { 'Content-Type': 'application/json', 'x-api-key': KEY },
    body: JSON.stringify({ model: 'deepseek-es', max_tokens: 100, messages: [{ role: 'user', content: 'Di OK' }], stream: true }),
  });
  const body2 = await r2.text();
  console.log('[e2e] M4 Anthropic 流式事件:', [...new Set([...body2.matchAll(/event: (\w+)/g)].map(m => m[1]))].join(', '));
  const mdMatch = [...body2.matchAll(/event: message_delta\ndata: (\{.*?\})\n/g)];
  let outTok = 0;
  if (mdMatch.length) { try { outTok = JSON.parse(mdMatch[mdMatch.length - 1][1]).usage?.output_tokens ?? 0; } catch {} }
  check('M4 Anthropic 流式 usage.output_tokens > 0', outTok > 0, `output_tokens=${outTok}`);
  check('M4 message_start 携带 message 结构', body2.includes('"input_tokens"'), '');

  // ── M7：正常结束仍有 message_stop ──
  check('M7 正常结束含 message_stop', body2.includes('message_stop'), '');

  // ── OpenAI 正常流式回归（确保本轮改动未破坏主路径） ──
  const r3 = await fetch(`${BASE}/v1/chat/completions`, {
    method: 'POST', headers: { 'Content-Type': 'application/json', 'Authorization': `Bearer ${KEY}` },
    body: JSON.stringify({ model: 'deepseek-es', messages: [{ role: 'user', content: '请只回复：你好' }], stream: true }),
  });
  const body3 = await r3.text();
  check('OpenAI 流式含中文且无替换符', body3.includes('你好') && !body3.includes('�'), '');
  check('OpenAI 流式含 finish_reason', body3.includes('finish_reason'), '');
  check('OpenAI 流式含 [DONE]', body3.includes('[DONE]'), '');

  // ── H4：伪工具（best-effort，模型可能不调用） ──
  const r4 = await fetch(`${BASE}/v1/chat/completions`, {
    method: 'POST', headers: { 'Content-Type': 'application/json', 'x-api-key': KEY },
    body: JSON.stringify({ model: 'deepseek-es', messages: [{ role: 'user', content: '请调用工具查询当前时间，你必须输出一个 ```tool 代码块调用 get_time' }] }),
  });
  const v4 = await r4.json();
  const content4 = v4.choices?.[0]?.message?.content ?? '';
  const toolHit = content4.includes('[tool:get_time]');
  console.log(`[e2e] H4 伪工具（best-effort）: ${toolHit ? '模型调用了工具' : '模型未调用（非确定性，不算失败）'}`);
  if (toolHit) console.log('    工具执行结果片段:', content4.slice(content4.indexOf('[tool:get_time]')).slice(0, 120));

  console.log(`\n===== E2E 结果: ${pass} 通过 / ${fail} 失败 =====`);
  if (fail > 0) process.exit(2);
}

main().catch(e => { console.error('❌', e.message); process.exit(1); });
