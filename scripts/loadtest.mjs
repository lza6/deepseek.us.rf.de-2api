#!/usr/bin/env node
/**
 * 网关压测：并发请求 /v1/chat/completions（非流式），测吞吐/错误率/延迟分位。
 *
 * 前置：网关运行中；cf_solver 运行中；已认证（首个请求会触发求解，较慢）。
 * 用法: node scripts/loadtest.mjs [并发数] [总请求数]
 *
 * 注意：上游 deepseek.es 有配额限制，压测请用较小规模（默认 4 并发 × 12 请求）。
 */
const BASE = process.env.GATEWAY_URL || 'http://127.0.0.1:47833';
const CONC = parseInt(process.argv[2] || '4', 10);
const TOTAL = parseInt(process.argv[3] || '12', 10);

const PROMPTS = [
  'Di hola', 'Di OK', 'Cuenta hasta 3', '¿2+2?', 'Di sí', 'Di no',
  'Escribe "test"', 'Di buenos días', 'Di adiós', 'Di gracias',
  'Responde solo con "1"', 'Responde solo con "2"',
];

async function one(i) {
  const t0 = Date.now();
  try {
    const r = await fetch(`${BASE}/v1/chat/completions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        model: 'deepseek-es',
        messages: [{ role: 'user', content: PROMPTS[i % PROMPTS.length] }],
        stream: false,
      }),
    });
    const ms = Date.now() - t0;
    if (!r.ok) return { ok: false, ms, status: r.status, err: (await r.text()).slice(0, 120) };
    const j = await r.json();
    const content = j.choices?.[0]?.message?.content || '';
    return { ok: true, ms, len: content.length, content };
  } catch (e) {
    return { ok: false, ms: Date.now() - t0, err: e.message };
  }
}

function pct(arr, p) {
  if (!arr.length) return 0;
  const s = [...arr].sort((a, b) => a - b);
  return s[Math.min(s.length - 1, Math.floor((p / 100) * s.length))];
}

async function main() {
  console.log(`[loadtest] 并发=${CONC} 总数=${TOTAL} @ ${BASE}`);
  // 预热（触发认证，避免把求解耗时算入统计）
  console.log('[loadtest] 预热（触发 Turnstile 认证）...');
  const warm = await one(0);
  console.log('[loadtest] 预热结果:', warm.ok ? 'OK' : 'FAIL: ' + warm.err, `${warm.ms}ms`);
  if (!warm.ok) { console.error('预热失败，终止'); process.exit(1); }

  const results = [];
  const start = Date.now();
  let idx = 0;
  async function worker() {
    while (true) {
      const i = idx++;
      if (i >= TOTAL) break;
      results.push(await one(i));
    }
  }
  await Promise.all(Array.from({ length: CONC }, worker));
  const elapsed = Date.now() - start;

  const ok = results.filter(r => r.ok);
  const fail = results.filter(r => !r.ok);
  const latencies = ok.map(r => r.ms);
  const qps = (results.length / (elapsed / 1000)).toFixed(2);

  console.log('\n===== 压测结果 =====');
  console.log(`总请求: ${results.length}`);
  console.log(`成功: ${ok.length} | 失败: ${fail.length}`);
  console.log(`总耗时: ${elapsed}ms | QPS: ${qps}`);
  console.log(`延迟 p50=${pct(latencies, 50)}ms p90=${pct(latencies, 90)}ms p99=${pct(latencies, 99)}ms max=${Math.max(0, ...latencies)}ms`);
  if (fail.length) {
    console.log('失败样例:');
    for (const f of fail.slice(0, 5)) console.log(`  - HTTP ${f.status || 'ERR'} ${f.err}`);
  }
  const successRate = ((ok.length / results.length) * 100).toFixed(1);
  console.log(`成功率: ${successRate}%`);

  // 断言：至少 70% 成功（上游有配额限制，不要求 100%）
  if (ok.length === 0) { console.error('❌ 全部失败'); process.exit(2); }
  console.log(ok.length === results.length ? '✅ 全部成功' : `⚠️ 部分失败（${fail.length}）`);
}

main().catch(e => { console.error(e); process.exit(1); });
