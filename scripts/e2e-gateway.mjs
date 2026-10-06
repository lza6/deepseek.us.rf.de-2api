#!/usr/bin/env node
/**
 * 网关真实 E2E：启动 Rust 网关 → 通过 OpenAI/Anthropic 端点真实聊天。
 *
 * 前置：
 *   1. cf_solver 运行中（http://127.0.0.1:8001）
 *   2. 网关已编译（cargo build --release）
 *
 * 用法: node scripts/e2e-gateway.mjs [问题]
 */
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..');
const BIN = path.join(ROOT, 'target', 'release', process.platform === 'win32' ? 'deepseek-es-2api.exe' : 'deepseek-es-2api');
const PORT = 47833;
const BASE = `http://127.0.0.1:${PORT}`;

function log(...a) { console.log('[e2e]', ...a); }

async function waitHealth(timeoutMs = 15000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const r = await fetch(`${BASE}/healthz`);
      if (r.ok) return await r.json();
    } catch {}
    await new Promise(s => setTimeout(s, 300));
  }
  throw new Error('网关未就绪');
}

async function main() {
  if (!fs.existsSync(BIN)) {
    console.error('未找到二进制，请先 cargo build --release:', BIN);
    process.exit(1);
  }

  // 写临时配置（走代理）
  const cfgPath = path.join(ROOT, 'target', 'e2e-config.json');
  const cfg = {
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
    http_timeout_secs: 120,
    cookie_ttl_secs: 1800,
    pseudo_chunk_chars: 0,
    cors_allow_origins: [],
  };
  fs.writeFileSync(cfgPath, JSON.stringify(cfg, null, 2));
  log('配置写入', cfgPath);

  const child = spawn(BIN, ['--config', cfgPath], {
    env: { ...process.env, RUST_LOG: 'info' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.stdout.on('data', d => process.stdout.write('[gw] ' + d));
  child.stderr.on('data', d => process.stdout.write('[gw!] ' + d));

  const cleanup = () => { try { child.kill(); } catch {} };
  process.on('exit', cleanup);

  try {
    const health = await waitHealth();
    log('网关就绪:', JSON.stringify(health));

    const prompt = process.argv[2] || 'Di hola en una frase';

    // 1) /v1/models
    const models = await (await fetch(`${BASE}/v1/models`)).json();
    log(`模型数: ${models.data.length}, 默认: ${models.data.find(m => m.default)?.id}`);

    // 2) OpenAI 流式
    log('--- OpenAI /v1/chat/completions (stream) ---');
    const t0 = Date.now();
    const r = await fetch(`${BASE}/v1/chat/completions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model: 'deepseek-es', messages: [{ role: 'user', content: prompt }], stream: true }),
    });
    log('HTTP', r.status, r.headers.get('content-type'));
    const reader = r.body.getReader();
    const dec = new TextDecoder();
    let buf = '', full = '', done = false, chunks = 0;
    while (!done) {
      const { done: d, value } = await reader.read();
      if (d) break;
      buf += dec.decode(value, { stream: true });
      const parts = buf.split('\n\n'); buf = parts.pop();
      for (const p of parts) {
        const line = p.trim();
        if (!line.startsWith('data:')) continue;
        const data = line.slice(5).trim();
        if (data === '[DONE]') { done = true; break; }
        try {
          const j = JSON.parse(data);
          const c = j.choices?.[0]?.delta?.content;
          if (c) { full += c; chunks++; }
        } catch {}
      }
    }
    log(`OpenAI 回复 (${chunks} chunks, ${Date.now() - t0}ms): ${JSON.stringify(full)}`);

    // 3) OpenAI 非流式
    log('--- OpenAI /v1/chat/completions (non-stream) ---');
    const r2 = await fetch(`${BASE}/v1/chat/completions`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model: 'deepseek-es', messages: [{ role: 'user', content: 'Di OK' }], stream: false }),
    });
    const j2 = await r2.json();
    log('HTTP', r2.status, '内容:', JSON.stringify(j2.choices?.[0]?.message?.content));

    // 4) Anthropic
    log('--- Anthropic /v1/messages (stream) ---');
    const r3 = await fetch(`${BASE}/v1/messages`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ model: 'deepseek-es', max_tokens: 100, messages: [{ role: 'user', content: 'Di OK' }], stream: true }),
    });
    log('HTTP', r3.status, r3.headers.get('content-type'));
    const b3 = await r3.text();
    const anthText = [...b3.matchAll(/data: (\{.*?\})\n/g)].map(m => {
      try { const j = JSON.parse(m[1]); return j.delta?.text || ''; } catch { return ''; }
    }).join('');
    log('Anthropic 回复:', JSON.stringify(anthText));
    log('Anthropic 事件类型:', [...new Set([...b3.matchAll(/event: (\w+)/g)].map(m => m[1]))].join(', '));

    fs.writeFileSync(path.join(ROOT, 'target', 'e2e-gateway-result.json'), JSON.stringify({
      openai_stream: full, openai_nonstream: j2.choices?.[0]?.message?.content,
      anthropic_stream: anthText, events: [...new Set([...b3.matchAll(/event: (\w+)/g)].map(m => m[1]))],
    }, null, 2));
    log('结果已保存 target/e2e-gateway-result.json');

    if (!full && !anthText) { console.error('❌ 无回复'); process.exit(2); }
    log('✅ E2E 通过');
  } finally {
    cleanup();
  }
}

main().catch(e => { console.error('❌', e.message); process.exit(1); });
