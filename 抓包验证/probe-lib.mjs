#!/usr/bin/env node
/**
 * 共享探测库：cookie jar / 表单 POST / 求解 / 缓存+SSE
 * 供 probe-*.mjs 复用。不修改 solve-and-chat.mjs。
 */
import fs from 'node:fs';

export const BASE = 'https://deepseek.es';
export const AJAX = BASE + '/wp-admin/admin-ajax.php';
export const SOLVER = process.env.CF_SOLVER_URL || 'http://127.0.0.1:8001';
export const SITEKEY = process.env.SITEKEY || '0x4AAAAAADlLZ3ljqZP6cQwq';
export const BOT_ID = process.env.BOT_ID || '27623';
export const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36';
export const JAR_FILE = 'probe-cookies.json';

export function loadJar() {
  try { return JSON.parse(fs.readFileSync(JAR_FILE, 'utf8')); } catch { return {}; }
}
export function saveJar(jar) { fs.writeFileSync(JAR_FILE, JSON.stringify(jar, null, 2)); }
export function cookieHeader(jar) {
  return Object.entries(jar).map(([k, v]) => `${k}=${v}`).join('; ');
}
export function absorbSetCookie(jar, resp) {
  const sc = resp.headers.getSetCookie ? resp.headers.getSetCookie() : [];
  for (const c of sc) {
    const [pair] = c.split(';');
    const idx = pair.indexOf('=');
    if (idx > 0) jar[pair.slice(0, idx).trim()] = pair.slice(idx + 1).trim();
  }
  return sc;
}

export function headers(jar, extra = {}) {
  const ch = cookieHeader(jar);
  return {
    'User-Agent': UA,
    'Accept': '*/*',
    'Accept-Language': 'es-ES,es;q=0.9,en;q=0.8',
    ...(ch ? { Cookie: ch } : {}),
    ...extra,
  };
}

export async function postForm(jar, params, extraHeaders = {}) {
  const r = await fetch(AJAX, {
    method: 'POST',
    headers: headers(jar, { 'Content-Type': 'application/x-www-form-urlencoded', ...extraHeaders }),
    body: new URLSearchParams(params).toString(),
  });
  const sc = absorbSetCookie(jar, r);
  const text = await r.text();
  let json = null; try { json = JSON.parse(text); } catch {}
  return { status: r.status, text, json, setCookie: sc };
}

export async function solveTurnstile() {
  console.log('[solve] 提交 Turnstile 任务...');
  const url = `${SOLVER}/turnstile?url=${encodeURIComponent(BASE + '/')}&sitekey=${encodeURIComponent(SITEKEY)}&action=chat`;
  const r = await fetch(url);
  const j = await r.json();
  if (!j.task_id) throw new Error('提交失败: ' + JSON.stringify(j));
  console.log('[solve] task_id:', j.task_id);
  for (let i = 0; i < 60; i++) {
    await new Promise(s => setTimeout(s, 3000));
    const rr = await fetch(`${SOLVER}/result?id=${j.task_id}`);
    const jj = await rr.json();
    if (jj.status === 'success') { console.log('[solve] 成功, token 长度:', jj.value.length); return jj.value; }
    if (jj.status === 'error' && !/not valid|expired/.test(String(jj.message || ''))) {
      throw new Error('求解失败: ' + JSON.stringify(jj));
    }
    process.stdout.write('.');
  }
  throw new Error('求解超时');
}

/** 完整求解并兑换 dsts_ok，返回 nonce；jar 就地更新 */
export async function ensureCookie(jar, { force = false } = {}) {
  if (!force && jar.dsts_ok) {
    console.log('[cookie] 复用已有 dsts_ok，jar:', cookieHeader(jar));
    return null;
  }
  await postForm(jar, { action: 'aipkit_get_frontend_chat_nonce', bot_id: BOT_ID });
  const token = await solveTurnstile();
  const verify = await postForm(jar, { action: 'deepseek_ts_verify', token });
  console.log('[cookie] verify HTTP', verify.status, verify.text.slice(0, 120));
  if (!verify.json?.ok) throw new Error('兑换失败: ' + verify.text);
  saveJar(jar);
  return token;
}

export async function getNonce(jar) {
  const nz = await postForm(jar, { action: 'aipkit_get_frontend_chat_nonce', bot_id: BOT_ID });
  return { nonce: nz.json?.data?.nonce, raw: nz };
}

export async function cacheMessage(jar, message, nonce) {
  return postForm(jar, { action: 'aipkit_cache_sse_message', message, _ajax_nonce: nonce, bot_id: BOT_ID });
}

/** 返回 {status, contentType, events:[{event,data,raw}], raw, full, elapsed} */
export async function streamChat(jar, { cacheKey, nonce, sessionId, conversationUuid, extraHeaders = {} }) {
  const sid = sessionId || conversationUuid || crypto.randomUUID();
  const conv = conversationUuid || sid;
  const url = `${AJAX}?action=aipkit_frontend_chat_stream&cache_key=${encodeURIComponent(cacheKey)}&bot_id=${BOT_ID}&session_id=${sid}&conversation_uuid=${conv}&_ajax_nonce=${nonce}&_ts=${Date.now()}`;
  const t0 = Date.now();
  const r = await fetch(url, { headers: headers(jar, { Accept: 'text/event-stream', ...extraHeaders }) });
  absorbSetCookie(jar, r);
  const ct = r.headers.get('content-type');
  let raw = '';
  if (r.body) {
    const reader = r.body.getReader();
    const dec = new TextDecoder();
    let buf = '';
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true });
    }
    raw = buf;
  } else {
    raw = await r.text();
  }
  const { events, full } = parseSSE(raw);
  return { status: r.status, contentType: ct, events, full, raw, elapsed: Date.now() - t0, url };
}

export function parseSSE(raw) {
  const events = [];
  let full = '';
  let buf = raw;
  let sep;
  while ((sep = buf.indexOf('\n\n')) >= 0) {
    const block = buf.slice(0, sep);
    buf = buf.slice(sep + 2);
    let evName = 'message';
    let dataRaw = '';
    let dataLines = [];
    for (const line of block.split('\n')) {
      const l = line.replace(/\r$/, '');
      if (l.startsWith(':')) { evName = '__comment__'; continue; }
      if (l.startsWith('event:')) evName = l.slice(6).trim();
      else if (l.startsWith('data:')) { dataRaw += l.slice(5).replace(/^\s/, ''); dataLines.push(l); }
    }
    if (!dataRaw && evName !== '__comment__') continue;
    let data; try { data = JSON.parse(dataRaw); } catch { data = dataRaw; }
    events.push({ event: evName, data, raw: block });
    if (evName === 'message' && data && typeof data.delta === 'string') full += data.delta;
  }
  return { events, full };
}

/** 高层：缓存 + 流式，一次调用返回结果 */
export async function chatOnce(jar, prompt, { sessionId, conversationUuid, nonce, extraHeaders } = {}) {
  let n = nonce;
  if (!n) { const g = await getNonce(jar); n = g.nonce; }
  const ck = await cacheMessage(jar, prompt, n);
  const cacheKey = ck.json?.data?.cache_key;
  if (!cacheKey) return { error: 'cache_failed', cacheRaw: ck.text };
  const res = await streamChat(jar, { cacheKey, nonce: n, sessionId, conversationUuid, extraHeaders });
  return { nonce: n, cacheKey, ...res };
}
