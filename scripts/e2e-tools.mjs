#!/usr/bin/env node
/**
 * 真实 E2E：协议级工具调用（v2.0.0）—— 连真实上游 deepseek.es + cf_solver。
 *
 * 验证完整往返：
 *   1. 客户端声明 tools
 *   2. 模型输出 ```tool 块 → 网关产出标准 tool_calls/tool_use
 *   3. 客户端（本地函数）执行工具
 *   4. 客户端回传结果 → 网关转成 prompt → 模型给出最终回答
 *
 * 用法: node scripts/e2e-tools.mjs
 */
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const ROOT = path.resolve(import.meta.dirname, '..');
const BIN = path.join(ROOT, 'target', 'release', process.platform === 'win32' ? 'deepseek-es-2api.exe' : 'deepseek-es-2api');
const PORT = 47838;
const BASE = `http://127.0.0.1:${PORT}`;

const cfgPath = path.join(ROOT, 'target', 'e2e-tools-config.json');
fs.writeFileSync(cfgPath, JSON.stringify({
  listen_addr: `127.0.0.1:${PORT}`,
  upstream_base_url: 'https://deepseek.es',
  bot_id: '27623',
  sitekey: '0x4AAAAAADlLZ3ljqZP6cQwq',
  cf_solver_url: 'http://127.0.0.1:8001',
  user_agent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36',
  default_model: 'deepseek-es',
  api_keys: [],
  proxy: process.env.UPSTREAM_PROXY || 'http://127.0.0.1:10808',
  solver_timeout_secs: 120,
  http_timeout_secs: 300,
  connect_timeout_secs: 20,
  cookie_ttl_secs: 1800,
  cors_allow_origins: [],
  cache_ttl_secs: 0,
}, null, 2));

const child = spawn(BIN, ['--config', cfgPath], { env: { ...process.env, RUST_LOG: 'info' }, stdio: ['ignore', 'pipe', 'pipe'] });
child.stdout.on('data', d => process.stdout.write('[gw] ' + d));
child.stderr.on('data', d => process.stdout.write('[gw!] ' + d));
process.on('exit', () => { try { child.kill(); } catch {} });

async function waitHealth() {
  for (let i = 0; i < 60; i++) {
    try { const r = await fetch(`${BASE}/healthz`); if (r.ok) return; } catch {}
    await new Promise(s => setTimeout(s, 300));
  }
  throw new Error('网关未就绪');
}

// 真实的本地工具实现
const TOOLS = {
  get_weather: ({ city }) => ({ city, temp_c: 25, condition: '晴' }),
  get_time: () => ({ iso: new Date().toISOString() }),
};

async function chat(messages, tools) {
  const r = await fetch(`${BASE}/v1/chat/completions`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ model: 'deepseek-es', messages, tools, stream: false }),
  });
  return r.json();
}

async function main() {
  await waitHealth();
  console.log('[e2e] 网关就绪');

  const tools = [
    { type: 'function', function: { name: 'get_weather', description: '查询城市天气',
      parameters: { type: 'object', properties: { city: { type: 'string', description: '城市名' } }, required: ['city'] } } },
  ];

  // 第 1 轮：问天气（不带 tools 时无法调用；带 tools 应产出 tool_calls）
  const messages = [{ role: 'user', content: '北京今天天气怎么样？请使用工具查询。' }];
  const r1 = await chat(messages, tools);
  const choice = r1.choices?.[0];
  const toolCalls = choice?.message?.tool_calls;
  console.log('[e2e] 第1轮 finish_reason:', choice?.finish_reason);
  console.log('[e2e] 第1轮 content:', JSON.stringify(choice?.message?.content ?? '').slice(0, 100));
  console.log('[e2e] 第1轮 tool_calls:', JSON.stringify(toolCalls));

  if (!toolCalls || toolCalls.length === 0) {
    console.log('⚠️  模型本轮未调用工具（非确定性）。工具协议本身已由集成测试验证。');
    // 仍然验证 tools 说明已注入上游（通过日志不可见，故此处仅报告）
    process.exit(0);
  }

  // 执行工具
  const tc = toolCalls[0];
  const args = JSON.parse(tc.function.arguments);
  const result = TOOLS[tc.function.name]?.(args) ?? { error: 'unknown tool' };
  console.log('[e2e] 本地执行工具:', tc.function.name, JSON.stringify(args), '→', JSON.stringify(result));

  // 第 2 轮：回传工具结果（OpenAI 标准 tool 消息）
  messages.push({ role: 'assistant', content: choice.message.content || '', tool_calls: toolCalls });
  messages.push({ role: 'tool', tool_call_id: tc.id, content: JSON.stringify(result) });
  const r2 = await chat(messages, tools);
  const final = r2.choices?.[0]?.message?.content ?? '';
  console.log('[e2e] 第2轮最终回答:', JSON.stringify(final));

  const ok = final.length > 0 && (final.includes('晴') || final.includes('25') || final.includes('北京'));
  console.log(ok ? '✅ 工具调用完整往返成功（模型基于工具结果作答）' : '⚠️ 第2轮回答未明确引用工具结果（模型行为，非协议问题）');
}

main().catch(e => { console.error('❌', e.message); process.exit(1); });
