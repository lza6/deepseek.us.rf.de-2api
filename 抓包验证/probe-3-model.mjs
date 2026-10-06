#!/usr/bin/env node
/**
 * 探测 3：模型身份
 * 用法: node probe-3-model.mjs
 */
import fs from 'node:fs';
import * as L from './probe-lib.mjs';

const jar = L.loadJar();
await L.ensureCookie(jar, { force: false });
L.saveJar(jar);

const prompts = [
  '¿Qué modelo de IA eres? Responde en una frase.',
  '¿Cuál es tu versión? Responde en una frase.',
  '¿Quién te creó y cuál es tu nombre interno? Responde en una frase.',
];
const log = [];
for (const p of prompts) {
  console.log(`\n[p3] Q: ${p}`);
  const r = await L.chatOnce(jar, p);
  console.log('[p3] A:', JSON.stringify(r.full), '| status', r.status, '| err', r.error || '-');
  log.push({ prompt: p, sseStatus: r.status, full: r.full, error: r.error, events: r.events?.map(e => e.event) });
  await new Promise(s => setTimeout(s, 2000));
}
fs.writeFileSync('probe-3-result.json', JSON.stringify({ probe: 'p3-model', log }, null, 2));
console.log('\n[p3] 已写 probe-3-result.json');
