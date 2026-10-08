#!/usr/bin/env node
/** collect-cookie 解析逻辑测试。用法: node scripts/collect-cookie.test.mjs */
import assert from 'node:assert/strict';
import { pickCookies, parseCookieFile, mask, toCookieHeader } from './collect-cookie.mjs';

let pass = 0, fail = 0;
const t = (n, f) => { try { f(); pass++; console.log(`✅ ${n}`); } catch (e) { fail++; console.log(`❌ ${n}: ${e.message}`); } };

t('pickCookies 提取 dsts_ok/dsts', () => {
  const r = pickCookies('dsts_ok=1; dsts=abc.def; other=x');
  assert.deepEqual(r, { dsts_ok: '1', dsts: 'abc.def' });
});
t('pickCookies 带 cf_clearance', () => {
  const r = pickCookies('dsts_ok=1; dsts=h; cf_clearance=cf123; junk=1');
  assert.equal(r.cf_clearance, 'cf123');
  assert.equal(r.junk, undefined);
});
t('pickCookies 忽略无 = 段与空段', () => {
  assert.deepEqual(pickCookies(' ; foo ; a=1 ; dsts_ok=1 '), { dsts_ok: '1' });
});
t('pickCookies 值内含 =', () => {
  assert.deepEqual(pickCookies('dsts=a=b=='), { dsts: 'a=b==' });
});

t('parseCookieFile JSON 数组', () => {
  const r = parseCookieFile(JSON.stringify([{ name: 'dsts_ok', value: '1' }, { name: 'x', value: 'y' }]));
  assert.deepEqual(r, { dsts_ok: '1' });
});
t('parseCookieFile JSON 对象', () => {
  const r = parseCookieFile(JSON.stringify({ dsts_ok: '1', dsts: 'h' }));
  assert.deepEqual(r, { dsts_ok: '1', dsts: 'h' });
});
t('parseCookieFile Netscape 格式', () => {
  const netscape = [
    '# Netscape HTTP Cookie File',
    '.deepseek.es\tTRUE\t/\tTRUE\t0\tdsts_ok\t1',
    '.deepseek.es\tTRUE\t/\tTRUE\t0\tdsts\thash123',
    '.other.com\tTRUE\t/\tFALSE\t0\tjunk\tz',
  ].join('\n');
  assert.deepEqual(parseCookieFile(netscape), { dsts_ok: '1', dsts: 'hash123' });
});

t('mask 不泄漏完整值', () => {
  assert.equal(mask('abcdefghij'), 'abcd…ij(len=10)');
  assert.equal(mask('abc'), '***(len=3)');
  assert.equal(mask(''), '(空)');
});
t('toCookieHeader 拼接', () => {
  assert.equal(toCookieHeader({ dsts_ok: '1', dsts: 'h' }), 'dsts_ok=1; dsts=h');
});

console.log(`\n===== ${pass} 通过 / ${fail} 失败 =====`);
process.exit(fail ? 2 : 0);
