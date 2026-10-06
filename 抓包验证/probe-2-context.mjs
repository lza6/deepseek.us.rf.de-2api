#!/usr/bin/env node
/**
 * 探测 2：多轮上下文是否由 conversation_uuid 维护
 * 固定 session_id/conversation_uuid，发 "我叫张三" → 再发 "我叫什么名字？"
 * 对照：换一个新 conversation_uuid 再问 "我叫什么名字？"（应不知道）
 * 用法: node probe-2-context.mjs
 */
import fs from 'node:fs';
import * as L from './probe-lib.mjs';

const jar = L.loadJar();
await L.ensureCookie(jar, { force: false });   // 复用 p1 的 cookie
L.saveJar(jar);
console.log('[p2] jar:', L.cookieHeader(jar));

const sid = crypto.randomUUID();
const log = [];

async function turn(label, prompt, sessionId, convUuid) {
  console.log(`\n[p2] --- ${label} --- (sid=${sessionId} conv=${convUuid})`);
  const r = await L.chatOnce(jar, prompt, { sessionId, conversationUuid: convUuid });
  console.log('[p2] status', r.status, '| 回复:', JSON.stringify(r.full), '| err:', r.error || '-');
  log.push({ label, prompt, sessionId, conversationUuid: convUuid, sseStatus: r.status, full: r.full, events: r.events?.map(e => e.event), error: r.error });
  return r;
}

// 轮1：同 conversation_uuid 建立上下文
await turn('T1 建立上下文', 'Me llamo ZhangSan. Recuérdalo.', sid, sid);
await new Promise(s => setTimeout(s, 2000));

// 轮2：同 conversation_uuid 追问
await turn('T2 同会话追问', '¿Cómo me llamo? Responde solo el nombre.', sid, sid);
await new Promise(s => setTimeout(s, 2000));

// 轮3：新 conversation_uuid 对照
const sid2 = crypto.randomUUID();
await turn('T3 新会话对照', '¿Cómo me llamo? Responde solo el nombre.', sid2, sid2);

fs.writeFileSync('probe-2-result.json', JSON.stringify({ probe: 'p2-context', log }, null, 2));
console.log('\n[p2] 已写 probe-2-result.json');
