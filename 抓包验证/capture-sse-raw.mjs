#!/usr/bin/env node
/** 抓取一次完整 SSE 原始原文，保存到 sse_raw.txt 供协议核对 */
import fs from 'node:fs';
import { execFileSync } from 'node:child_process';

const BASE = 'https://deepseek.es';
const AJAX = BASE + '/wp-admin/admin-ajax.php';
const BOT_ID = '27623';
const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36';

const COOKIE = 'dsts=' + (process.env.DSTS || '') + '; dsts_ok=1';
// 简单：直接复用 cookies.txt（若存在）
let cookieHeader = COOKIE;
try {
  const jar = fs.readFileSync(new URL('./cookies.txt', import.meta.url), 'utf8');
  cookieHeader = jar.split('\n').filter(l => l && !l.startsWith('#')).map(l => {
    const p = l.split('\t'); return p.length >= 7 ? `${p[5]}=${p[6]}` : null;
  }).filter(Boolean).join('; ');
} catch {}

async function post(params) {
  const r = await fetch(AJAX, { method: 'POST', headers: { 'User-Agent': UA, Origin: BASE, Referer: BASE + '/', 'Content-Type': 'application/x-www-form-urlencoded', ...(cookieHeader ? { Cookie: cookieHeader } : {}) }, body: new URLSearchParams(params).toString() });
  const sc = r.headers.getSetCookie ? r.headers.getSetCookie() : [];
  for (const c of sc) { const [pair] = c.split(';'); const i = pair.indexOf('='); if (i > 0) { const k = pair.slice(0, i).trim(), v = pair.slice(i + 1).trim(); cookieHeader = cookieHeader.replace(new RegExp(k + '=[^;]*'), '').replace(/; ?$/, '') + '; ' + k + '=' + v; } }
  return { status: r.status, json: await r.json().catch(() => null) };
}

const nonce = (await post({ action: 'aipkit_get_frontend_chat_nonce', bot_id: BOT_ID })).json?.data?.nonce;
const ck = await post({ action: 'aipkit_cache_sse_message', message: process.argv[2] || 'Say hi', _ajax_nonce: nonce, bot_id: BOT_ID });
const key = ck.json?.data?.cache_key;
console.log('nonce=' + nonce, 'key=' + key);
const sid = crypto.randomUUID();
const url = `${AJAX}?action=aipkit_frontend_chat_stream&cache_key=${key}&bot_id=${BOT_ID}&session_id=${sid}&conversation_uuid=${sid}&_ajax_nonce=${nonce}&_ts=${Date.now()}`;

// 用 curl 抓原文（可显式 --noproxy 控制）
const out = execFileSync('curl', ['-s', '--ssl-no-revoke', '--max-time', '60', '-N', url,
  '-H', 'User-Agent: ' + UA, '-H', 'Accept: text/event-stream', '-H', 'Origin: ' + BASE,
  '-H', 'Referer: ' + BASE + '/', '-H', 'Cookie: ' + cookieHeader], { maxBuffer: 10 * 1024 * 1024 });
fs.writeFileSync('sse_raw.txt', out);
console.log('saved sse_raw.txt', out.length, 'bytes');
console.log('--- 原文（制表符可视化为 \\t）---');
console.log(out.toString('utf8').replace(/\r/g, '\\r').replace(/\t/g, '\\t').split('\n').slice(0, 40).join('\n'));
