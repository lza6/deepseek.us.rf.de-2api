#!/usr/bin/env node
/**
 * 探测 4：SSE 事件全集 + 原始字节（含填充注释）
 * 用法: node probe-4-sse-events.mjs
 */
import fs from 'node:fs';
import * as L from './probe-lib.mjs';

const jar = L.loadJar();
await L.ensureCookie(jar, { force: false });
L.saveJar(jar);

// 用较长回复以覆盖多个 delta 与事件
const prompt = 'Escribe una lista de 5 colores, uno por línea, y nada más.';
console.log('[p4] prompt:', prompt);

const g = await L.getNonce(jar);
const nonce = g.nonce;
const ck = await L.cacheMessage(jar, prompt, nonce);
const cacheKey = ck.json?.data?.cache_key;
console.log('[p4] nonce', nonce, 'cache_key', cacheKey);
if (!cacheKey) throw new Error('cache failed: ' + ck.text);

const sseUrl = `${L.AJAX}?action=aipkit_frontend_chat_stream&cache_key=${encodeURIComponent(cacheKey)}&bot_id=${L.BOT_ID}&session_id=${crypto.randomUUID()}&conversation_uuid=${crypto.randomUUID()}&_ajax_nonce=${nonce}&_ts=${Date.now()}`;
// 带 Origin/Referer，正常请求
const r = await fetch(sseUrl, { headers: L.headers(jar, { Accept: 'text/event-stream', Origin: L.BASE, Referer: L.BASE + '/' }) });
L.absorbSetCookie(jar, r);
const rawBytes = Buffer.from(await r.arrayBuffer());
fs.writeFileSync('probe-4-sse-raw.txt', rawBytes);
console.log('[p4] HTTP', r.status, '| content-type', r.headers.get('content-type'), '| bytes', rawBytes.length);
console.log('[p4] 响应头全集:');
for (const [k, v] of r.headers.entries()) console.log('   ', k, '=', v);

const raw = rawBytes.toString('utf8');
const { events, full } = L.parseSSE(raw);

// 统计
const nameCount = {};
for (const e of events) nameCount[e.event] = (nameCount[e.event] || 0) + 1;
console.log('\n[p4] 事件名统计:', JSON.stringify(nameCount));
console.log('[p4] 事件序列:', events.map(e => e.event).join(' → '));
console.log('[p4] 回复:', JSON.stringify(full));

// 填充注释分析：以 ":" 开头的行
const lines = raw.split('\n');
const commentLines = lines.filter(l => l.startsWith(':'));
console.log('\n[p4] 注释/填充行数:', commentLines.length);
if (commentLines.length) {
  console.log('[p4] 首条填充长度:', commentLines[0].length, '内容长度(去冒号):', commentLines[0].length - 1);
  console.log('[p4] 填充是否全空白:', /^:\s*$/.test(commentLines[0]));
}
// 原始行预览（可视制表符/回车）
console.log('\n[p4] 原始前 25 行（\\t/\\r 可视化）:');
console.log(lines.slice(0, 25).map(l => l.replace(/\r/g, '\\r').replace(/\t/g, '\\t').slice(0, 120)).join('\n'));

fs.writeFileSync('probe-4-result.json', JSON.stringify({
  probe: 'p4-sse-events', sseStatus: r.status, contentType: r.headers.get('content-type'),
  bytes: rawBytes.length, nameCount, eventSequence: events.map(e => e.event),
  events: events.map(e => ({ event: e.event, data: e.data })), full,
  commentLines: commentLines.length, commentLooksWhitespace: commentLines[0] ? /^:\s*$/.test(commentLines[0]) : null,
  responseHeaders: Object.fromEntries(r.headers.entries()),
}, null, 2));
console.log('\n[p4] 已写 probe-4-result.json + probe-4-sse-raw.txt');
