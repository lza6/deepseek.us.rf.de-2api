#!/usr/bin/env node
/**
 * 探测 5：SSE 请求是否必需 Origin / Referer 头
 * A: 全头（Origin+Referer） B: 无 Origin/Referer C: 仅 Origin D: 无 Cookie 对照
 * 用法: node probe-5-origin-referer.mjs
 */
import fs from 'node:fs';
import * as L from './probe-lib.mjs';

const jar = L.loadJar();
await L.ensureCookie(jar, { force: false });
L.saveJar(jar);

const g = await L.getNonce(jar);
const nonce = g.nonce;
const prompt = 'Di "ok" y nada más.';

const cases = [
  { name: 'A 全头(Origin+Referer)', omitOrigin: false, omitReferer: false, omitCookie: false },
  { name: 'B 无Origin+无Referer', omitOrigin: true, omitReferer: true, omitCookie: false },
  { name: 'C 仅Origin(无Referer)', omitOrigin: false, omitReferer: true, omitCookie: false },
  { name: 'D 全头但无Cookie', omitOrigin: false, omitReferer: false, omitCookie: true },
];
const log = [];

for (const c of cases) {
  console.log(`\n[p5] === ${c.name} ===`);
  const ck = await L.cacheMessage(jar, prompt, nonce);
  const cacheKey = ck.json?.data?.cache_key;
  if (!cacheKey) { console.log('[p5] cache 失败:', ck.text.slice(0, 150)); log.push({ case: c.name, error: 'cache_failed', raw: ck.text }); continue; }
  const h = L.headers(jar, { Accept: 'text/event-stream' });
  if (c.omitOrigin) delete h.Origin;
  if (c.omitReferer) delete h.Referer;
  if (c.omitCookie) delete h.Cookie;
  const url = `${L.AJAX}?action=aipkit_frontend_chat_stream&cache_key=${encodeURIComponent(cacheKey)}&bot_id=${L.BOT_ID}&session_id=${crypto.randomUUID()}&conversation_uuid=${crypto.randomUUID()}&_ajax_nonce=${nonce}&_ts=${Date.now()}`;
  const t0 = Date.now();
  const r = await fetch(url, { headers: h });
  const raw = await r.text();
  const ms = Date.now() - t0;
  console.log('[p5] HTTP', r.status, '| ct', r.headers.get('content-type'), '| 用时', ms + 'ms', '| bytes', raw.length);
  console.log('[p5] body 前 200:', JSON.stringify(raw.slice(0, 200)));
  const { events, full } = L.parseSSE(raw);
  log.push({ case: c.name, status: r.status, contentType: r.headers.get('content-type'), ms, bytes: raw.length, bodyHead: raw.slice(0, 300), events: events.map(e => e.event), full });
  await new Promise(s => setTimeout(s, 1500));
}

fs.writeFileSync('probe-5-result.json', JSON.stringify({ probe: 'p5-origin-referer', log }, null, 2));
console.log('\n[p5] 已写 probe-5-result.json');
