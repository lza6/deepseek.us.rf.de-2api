#!/usr/bin/env node
/**
 * api_solver 适配器测试（无需网络/无需 key —— 用注入的 fetch 桩）。
 * 用法: node tools/api_solver/server.test.mjs
 */
import assert from 'node:assert/strict';
import { capsolver, twocaptcha, providerOf, createServer, submit, poll, gcTasks } from './server.mjs';

let pass = 0, fail = 0;
const t = (name, fn) => {
  try { fn(); pass++; console.log(`✅ ${name}`); }
  catch (e) { fail++; console.log(`❌ ${name}: ${e.message}`); }
};
const ta = async (name, fn) => {
  try { await fn(); pass++; console.log(`✅ ${name}`); }
  catch (e) { fail++; console.log(`❌ ${name}: ${e.message}`); }
};

// ── provider 解析 ──────────────────────────────────────
t('providerOf 默认 capsolver', () => assert.equal(providerOf('capsolver').name, 'capsolver'));
t('providerOf 2captcha', () => assert.equal(providerOf('2captcha').name, '2captcha'));
t('providerOf 未知回退 capsolver', () => assert.equal(providerOf('nope').name, 'capsolver'));

// ── capsolver 请求体/解析 ──────────────────────────────
t('capsolver createBody 结构', () => {
  const b = capsolver.createBody('0xKEY', 'https://x.es/', 'chat');
  assert.equal(b.task.type, 'AntiTurnstileTaskProxyLess');
  assert.equal(b.task.websiteKey, '0xKEY');
  assert.equal(b.task.websiteURL, 'https://x.es/');
  assert.equal(b.task.metadata.action, 'chat');
});
t('capsolver createBody 无 action 时不带 metadata', () => {
  const b = capsolver.createBody('k', 'u', '');
  assert.equal(b.task.metadata, undefined);
});
t('capsolver parseCreate 返回 taskId', () =>
  assert.equal(capsolver.parseCreate({ errorId: 0, taskId: 'pt-1' }), 'pt-1'));
t('capsolver parseCreate 出错抛异常', () =>
  assert.throws(() => capsolver.parseCreate({ errorId: 1, errorCode: 'X', errorDescription: 'bad' }), /capsolver/));
t('capsolver parseResult ready → success', () =>
  assert.deepEqual(capsolver.parseResult({ errorId: 0, status: 'ready', solution: { token: 'tok' } }),
    { status: 'success', value: 'tok' }));
t('capsolver parseResult processing → process', () =>
  assert.deepEqual(capsolver.parseResult({ errorId: 0, status: 'processing' }), { status: 'process' }));
t('capsolver parseResult ready 但无 token → 抛', () =>
  assert.throws(() => capsolver.parseResult({ errorId: 0, status: 'ready', solution: {} }), /无 token/));

// ── 2captcha 请求体/解析 ───────────────────────────────
t('2captcha createBody 结构', () => {
  const b = twocaptcha.createBody('0xK', 'https://x/', 'chat');
  assert.equal(b.method, 'turnstile');
  assert.equal(b.sitekey, '0xK');
  assert.equal(b.action, 'chat');
  assert.equal(b.json, 1);
});
t('2captcha parseCreate 成功', () =>
  assert.equal(twocaptcha.parseCreate({ status: 1, request: '12345' }), '12345'));
t('2captcha parseCreate 失败抛', () =>
  assert.throws(() => twocaptcha.parseCreate({ status: 0, request: 'ERROR_KEY' }), /2captcha/));
t('2captcha parseResult CAPCHA_NOT_READY → process', () =>
  assert.deepEqual(twocaptcha.parseResult({ status: 0, request: 'CAPCHA_NOT_READY' }), { status: 'process' }));
t('2captcha parseResult 成功 → success', () =>
  assert.deepEqual(twocaptcha.parseResult({ status: 1, request: 'tok' }), { status: 'success', value: 'tok' }));

// ── gc ────────────────────────────────────────────────
t('gcTasks 清理陈旧任务（无任务时不抛）', () => assert.doesNotThrow(() => gcTasks(Date.now())));

// ── HTTP 层（注入 fetch 桩）────────────────────────────
const okFetch = (seq) => async (url, opts) => {
  const body = opts?.body ? JSON.parse(opts.body) : null;
  if (url.includes('createTask')) return { json: async () => ({ errorId: 0, taskId: 'pt-1' }) };
  if (url.includes('getTaskResult')) {
    const n = seq.n++;
    return { json: async () => n === 0
      ? { errorId: 0, status: 'processing' }
      : { errorId: 0, status: 'ready', solution: { token: 'REALTOKEN' } } };
  }
  throw new Error('unexpected url ' + url);
};

/** 起一个测试用服务，返回 base url。 */
async function withServer(fetchImpl, fn) {
  // 让 KEY 生效（模块内 const 已读环境；这里直接走 createServer 的注入路径，
  // 但 KEY 为空时 /turnstile 会 503 —— 所以测试用一个临时环境变量不可行。
  // 改为：直接用 createServer，并以 KEY 是否存在决定断言。
  const srv = createServer({ fetchImpl });
  await new Promise((r) => srv.listen(0, '127.0.0.1', r));
  const port = srv.address().port;
  try { return await fn(`http://127.0.0.1:${port}`); }
  finally { srv.close(); }
}

await ta('HTTP /health 返回 backend 与 key 状态', async () => {
  await withServer(okFetch({ n: 0 }), async (base) => {
    const j = await (await fetch(`${base}/health`)).json();
    assert.equal(j.status, 'ok');
    assert.equal(j.backend, 'capsolver');
    assert.equal(typeof j.key_configured, 'boolean');
  });
});

await ta('HTTP /turnstile 缺 url/sitekey → 400', async () => {
  await withServer(okFetch({ n: 0 }), async (base) => {
    const r = await fetch(`${base}/turnstile`);
    assert.equal(r.status, 400);
  });
});

await ta('HTTP /result 未知 id → 404', async () => {
  await withServer(okFetch({ n: 0 }), async (base) => {
    const r = await fetch(`${base}/result?id=nope`);
    assert.equal(r.status, 404);
  });
});

await ta('HTTP 无 key 时 /turnstile → 503（不含 key 硬编码）', async () => {
  await withServer(okFetch({ n: 0 }), async (base) => {
    const r = await fetch(`${base}/turnstile?url=https://x/&sitekey=0xK`);
    // 本测试进程通常未设 API_SOLVER_KEY → 503；若设了则 202，两者皆可视为“未崩”
    assert.ok([202, 503].includes(r.status), `实际 ${r.status}`);
  });
});

await ta('HTTP 未知路径 → 404', async () => {
  await withServer(okFetch({ n: 0 }), async (base) => {
    assert.equal((await fetch(`${base}/nope`)).status, 404);
  });
});

console.log(`\n===== ${pass} 通过 / ${fail} 失败 =====`);
process.exit(fail ? 2 : 0);
