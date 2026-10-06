#!/usr/bin/env node
/**
 * deepseek.es 完整聊天 E2E 验证脚本
 * 全链路：nonce → cache_sse_message → SSE 流 → 解析真实回复
 * 前置：已通过 cf_solver 获取 dsts_ok cookie（见 solve-and-chat.mjs）
 */
import fs from 'node:fs';

// 代理：Node v25 支持 NODE_USE_ENV_PROXY=1 内建读取 HTTPS_PROXY/HTTP_PROXY
console.log('[0] 代理:', process.env.HTTPS_PROXY || process.env.HTTP_PROXY || '(直连)');

const BASE = 'https://deepseek.es';
const AJAX = BASE + '/wp-admin/admin-ajax.php';
const BOT_ID = process.env.BOT_ID || '27623';
const COOKIE_FILE = process.env.COOKIE_FILE || 'cookies.txt';
const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36';

function parseCookieJar(text) {
  // Netscape cookie file → Cookie header
  return text.split('\n')
    .filter(l => l && !l.startsWith('#'))
    .map(l => {
      const p = l.split('\t');
      return p.length >= 7 ? `${p[5]}=${p[6]}` : null;
    })
    .filter(Boolean)
    .join('; ');
}

function headers(cookie) {
  return {
    'User-Agent': UA,
    'Accept': '*/*',
    'Origin': BASE,
    'Referer': BASE + '/',
    'Accept-Language': 'es-ES,es;q=0.9,en;q=0.8',
    ...(cookie ? { Cookie: cookie } : {}),
  };
}

async function form(url, cookie, params) {
  const body = new URLSearchParams(params).toString();
  const r = await fetch(url, {
    method: 'POST',
    headers: { ...headers(cookie), 'Content-Type': 'application/x-www-form-urlencoded' },
    body,
    // Node 18+ fetch: TLS. 若 cert 问题设置 NODE_TLS_REJECT_UNAUTHORIZED=0
  });
  const text = await r.text();
  let json = null;
  try { json = JSON.parse(text); } catch {}
  return { status: r.status, text, json };
}

async function main() {
  const cookie = fs.existsSync(COOKIE_FILE) ? parseCookieJar(fs.readFileSync(COOKIE_FILE, 'utf8')) : '';
  console.log('[1] 使用 cookie:', cookie || '(无，可能被安全校验拦截)');

  // 1) 刷新 nonce
  const nz = await form(AJAX, cookie, { action: 'aipkit_get_frontend_chat_nonce', bot_id: BOT_ID });
  const nonce = nz.json?.data?.nonce;
  console.log('[2] nonce:', nonce, '(HTTP', nz.status + ')');
  if (!nonce) { console.error('nonce 获取失败:', nz.text); process.exit(1); }

  // 2) 缓存消息
  const msg = process.argv[2] || 'Di hola en una frase';
  const ck = await form(AJAX, cookie, { action: 'aipkit_cache_sse_message', message: msg, _ajax_nonce: nonce, bot_id: BOT_ID });
  const cacheKey = ck.json?.data?.cache_key;
  console.log('[3] cache_key:', cacheKey, '(HTTP', ck.status + ')');
  if (!cacheKey) { console.error('缓存失败:', ck.text); process.exit(1); }

  // 3) 建立 SSE 流
  const sid = crypto.randomUUID();
  const url = `${AJAX}?action=aipkit_frontend_chat_stream&cache_key=${encodeURIComponent(cacheKey)}&bot_id=${BOT_ID}&session_id=${sid}&conversation_uuid=${sid}&_ajax_nonce=${nonce}&_ts=${Date.now()}`;
  console.log('[4] SSE 请求 sid:', sid);
  const r = await fetch(url, { headers: { ...headers(cookie), Accept: 'text/event-stream' } });
  console.log('[5] SSE HTTP', r.status, r.headers.get('content-type'));

  const reader = r.body.getReader();
  const decoder = new TextDecoder();
  let buf = '';
  let full = '';
  const events = [];
  let curEvent = 'message';

  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    const lines = buf.split('\n');
    buf = lines.pop();
    for (const line of lines) {
      if (line.startsWith('event:')) curEvent = line.slice(6).trim();
      else if (line.startsWith('data:')) {
        const raw = line.slice(5).trim();
        let data = null;
        try { data = JSON.parse(raw); } catch { data = raw; }
        events.push({ event: curEvent, data });
        if (curEvent === 'message' && data && typeof data.delta === 'string') full += data.delta;
        if (process.env.VERBOSE) console.log(`  [${curEvent}]`, raw.slice(0, 120));
      }
    }
  }

  console.log('\n[6] 事件序列:', events.map(e => e.event).join(' → '));
  console.log('[7] 完整回复:', JSON.stringify(full));
  fs.writeFileSync('e2e_result.json', JSON.stringify({ nonce, cacheKey, sid, events, full }, null, 2));
  console.log('[8] 已保存 e2e_result.json');
}

main().catch(e => { console.error('E2E 失败:', e); process.exit(1); });
