#!/usr/bin/env node
/**
 * 探测 1：dsts_ok cookie 有效期 / 一次会话内可复用次数
 * 流程：求解一次 → 连续 cache+SSE N 次（不同 session_id），记录每次结果与时间戳。
 * 用法: node probe-1-cookie-reuse.mjs [次数] [间隔秒]
 */
import fs from 'node:fs';
import * as L from './probe-lib.mjs';

const N = parseInt(process.argv[2] || '5', 10);
const GAP = parseInt(process.argv[3] || '3', 10);

const jar = L.loadJar();
const t0 = Date.now();
const log = [];

// 强制重新求解一次，确保 cookie 新鲜
await L.ensureCookie(jar, { force: true });
L.saveJar(jar);
const cookieAcquiredAt = new Date().toISOString();
console.log('[p1] cookie 获取于', cookieAcquiredAt, '|', L.cookieHeader(jar));

for (let i = 1; i <= N; i++) {
  const t = Date.now();
  const el = Math.round((Date.now() - t0) / 1000);
  console.log(`\n[p1] === 第 ${i}/${N} 次 @ +${el}s ===`);
  try {
    const r = await L.chatOnce(jar, `Di exactamente el número ${i} y nada más.`);
    const row = {
      n: i, atSec: el, iso: new Date().toISOString(),
      sseStatus: r.status, ct: r.contentType, cacheKey: r.cacheKey,
      events: r.events?.map(e => e.event),
      full: r.full, elapsed: r.elapsed,
      error: r.error, cacheRaw: r.error ? r.cacheRaw : undefined,
    };
    console.log('[p1] SSE', r.status, '| 事件:', row.events?.join(',') || '(无)', '| 回复:', JSON.stringify(r.full), '| 用时', r.elapsed + 'ms');
    if (r.error) console.log('[p1] ⚠️ 失败:', r.error, r.cacheRaw?.slice(0, 200));
    log.push(row);
    if (r.error || !r.full) {
      // 判失败：可能 cookie 失效
      console.log('[p1] ❌ 第', i, '次失败，停止连续测试');
      break;
    }
  } catch (e) {
    console.log('[p1] ❌ 异常:', e.message);
    log.push({ n: i, atSec: el, iso: new Date().toISOString(), exception: e.message });
    break;
  }
  if (i < N) await new Promise(s => setTimeout(s, GAP * 1000));
}

const ok = log.filter(r => r.full && !r.error).length;
const summary = { probe: 'p1-cookie-reuse', cookieAcquiredAt, total: N, success: ok, log };
fs.writeFileSync('probe-1-result.json', JSON.stringify(summary, null, 2));
console.log(`\n[p1] 汇总: ${ok}/${N} 成功 | jar 最终:`, L.cookieHeader(jar));
console.log('[p1] 已写 probe-1-result.json');
