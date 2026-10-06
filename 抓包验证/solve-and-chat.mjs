#!/usr/bin/env node
/**
 * deepseek.es 完整 E2E：cf_solver 求解 Turnstile → dsts_ok cookie → 真实聊天
 * 用法: node solve-and-chat.mjs "你的问题"
 */
import fs from 'node:fs';

const BASE = 'https://deepseek.es';
const AJAX = BASE + '/wp-admin/admin-ajax.php';
const SOLVER = process.env.CF_SOLVER_URL || 'http://127.0.0.1:8001';
const SITEKEY = process.env.SITEKEY || '0x4AAAAAADlLZ3ljqZP6cQwq';
const BOT_ID = process.env.BOT_ID || '27623';
const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36';

let COOKIE_JAR = {};   // name → value
function cookieHeader() {
  return Object.entries(COOKIE_JAR).map(([k, v]) => `${k}=${v}`).join('; ');
}
function absorbSetCookie(resp) {
  // Node fetch: getSetCookie()
  const sc = resp.headers.getSetCookie ? resp.headers.getSetCookie() : [];
  for (const c of sc) {
    const [pair] = c.split(';');
    const idx = pair.indexOf('=');
    if (idx > 0) COOKIE_JAR[pair.slice(0, idx).trim()] = pair.slice(idx + 1).trim();
  }
}
function baseHeaders(extra = {}) {
  return {
    'User-Agent': UA, 'Accept': '*/*', 'Origin': BASE, 'Referer': BASE + '/',
    'Accept-Language': 'es-ES,es;q=0.9,en;q=0.8',
    ...(cookieHeader() ? { Cookie: cookieHeader() } : {}),
    ...extra,
  };
}
async function postForm(params) {
  const r = await fetch(AJAX, {
    method: 'POST',
    headers: baseHeaders({ 'Content-Type': 'application/x-www-form-urlencoded' }),
    body: new URLSearchParams(params).toString(),
  });
  absorbSetCookie(r);
  const text = await r.text();
  let json = null; try { json = JSON.parse(text); } catch {}
  return { status: r.status, text, json };
}

async function solveTurnstile() {
  console.log('[t1] 提交 Turnstile 求解任务...');
  const url = `${SOLVER}/turnstile?url=${encodeURIComponent(BASE + '/')}&sitekey=${encodeURIComponent(SITEKEY)}&action=chat`;
  const r = await fetch(url);
  const j = await r.json();
  if (!j.task_id) throw new Error('求解任务提交失败: ' + JSON.stringify(j));
  const taskId = j.task_id;
  console.log('[t2] task_id:', taskId);
  for (let i = 0; i < 40; i++) {
    await new Promise(s => setTimeout(s, 3000));
    const rr = await fetch(`${SOLVER}/result?id=${taskId}`);
    const jj = await rr.json();
    if (jj.status === 'success') { console.log('[t3] 求解成功, token 长度:', jj.value.length); return jj.value; }
    if (jj.status === 'error' || rr.status >= 400) {
      if (String(jj.message || '').includes('not valid') || String(jj.message || '').includes('expired')) continue;
      if (jj.status === 'error') throw new Error('求解失败: ' + JSON.stringify(jj));
    }
    process.stdout.write('.');
  }
  throw new Error('求解超时');
}

async function main() {
  const prompt = process.argv[2] || 'Di hola en una frase';

  // 0) 预热：拿初始 cookie + nonce
  await postForm({ action: 'aipkit_get_frontend_chat_nonce', bot_id: BOT_ID });

  // 1) 求解并兑换 dsts_ok
  const token = await solveTurnstile();
  const verify = await postForm({ action: 'deepseek_ts_verify', token });
  console.log('[t4] deepseek_ts_verify:', verify.status, verify.text, '| cookie:', cookieHeader());
  if (!verify.json?.ok) throw new Error('安全校验兑换失败: ' + verify.text);

  // 2) 刷新 nonce（兑换后）
  const nz = await postForm({ action: 'aipkit_get_frontend_chat_nonce', bot_id: BOT_ID });
  const nonce = nz.json?.data?.nonce;
  console.log('[c1] nonce:', nonce);

  // 3) 缓存消息
  const ck = await postForm({ action: 'aipkit_cache_sse_message', message: prompt, _ajax_nonce: nonce, bot_id: BOT_ID });
  const cacheKey = ck.json?.data?.cache_key;
  console.log('[c2] cache_key:', cacheKey);
  if (!cacheKey) throw new Error('缓存失败: ' + ck.text);

  // 4) SSE
  const sid = crypto.randomUUID();
  const sseUrl = `${AJAX}?action=aipkit_frontend_chat_stream&cache_key=${encodeURIComponent(cacheKey)}&bot_id=${BOT_ID}&session_id=${sid}&conversation_uuid=${sid}&_ajax_nonce=${nonce}&_ts=${Date.now()}`;
  console.log('[c3] 建立 SSE...');
  const r = await fetch(sseUrl, { headers: baseHeaders({ Accept: 'text/event-stream' }) });
  console.log('[c4] SSE HTTP', r.status);
  absorbSetCookie(r);

  const reader = r.body.getReader();
  const dec = new TextDecoder();
  let buf = '', full = '', curEvent = null;
  const events = [];
  // SSE 分隔单位是「空行」。规范解析：遇到空行才提交一个事件。
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += dec.decode(value, { stream: true });
    let sep;
    while ((sep = buf.indexOf('\n\n')) >= 0) {
      const block = buf.slice(0, sep);
      buf = buf.slice(sep + 2);
      let evName = 'message';   // 无 event: 行 = 默认 message 事件（承载 delta）
      let dataRaw = '';
      for (const line of block.split('\n')) {
        if (line.startsWith('event:')) evName = line.slice(6).trim();
        else if (line.startsWith('data:')) dataRaw += line.slice(5).trim();
      }
      if (!dataRaw) continue;
      let data; try { data = JSON.parse(dataRaw); } catch { data = dataRaw; }
      events.push({ event: evName, data });
      if (evName === 'message' && data && typeof data.delta === 'string') full += data.delta;
    }
  }
  console.log('[c5] 事件序列:', events.map(e => e.event).join(' → '));
  console.log('[c6] 回复:', JSON.stringify(full));
  fs.writeFileSync('solve-and-chat-result.json', JSON.stringify({ prompt, nonce, cacheKey, sid, events, full }, null, 2));
  if (!full) { console.log('⚠️ 无文本回复，原始事件:', JSON.stringify(events)); process.exit(2); }
}

main().catch(e => { console.error('❌', e.message); process.exit(1); });
