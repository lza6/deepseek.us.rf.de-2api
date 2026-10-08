#!/usr/bin/env node
/**
 * api_solver — 把 cf_solver 契约翻译到第三方 captcha API（capsolver / 2captcha）。
 *
 * ## 为什么要它
 *
 * 本项目默认用 `tools/cf_solver`（camoufox 浏览器）求解 Cloudflare Turnstile。
 * 浏览器方案有两类问题：
 *   1. 起不来就全停（浏览器损坏、系统资源、**网络出口断**）；
 *   2. 需要图形环境/较多资源，扩展性差。
 *
 * 本适配器提供**纯 HTTP** 的求解后端：网关无需改动，把 `solver_urls` 指向本服务即可，
 * 与 camoufox 实例**并存做故障转移**。
 *
 * ## 契约（与 cf_solver 完全一致，见 src/solver.rs）
 *
 *   GET /turnstile?url=&sitekey=[&action=]  → 202 {task_id, status:"accepted"}
 *   GET /result?id=<task_id>                → 200 {status:"success", value:<token>}
 *                                           → 200 {status:"process"}   求解中
 *                                           → 404 未知/过期
 *                                           → 429 限流
 *   GET /health                             → 200 {status:"ok", backend:..}
 *
 * ## 配置（环境变量）
 *
 *   API_SOLVER_PROVIDER  capsolver | 2captcha   （默认 capsolver）
 *   API_SOLVER_KEY       第三方 API key（必填，否则 /turnstile 返回 503）
 *   API_SOLVER_PORT      监听端口（默认 8002，避免与 cf_solver 的 8001 冲突）
 *   API_SOLVER_POLL_MS   轮询间隔（默认 3000）
 *   API_SOLVER_TIMEOUT_MS 单任务超时（默认 120000）
 *
 * ## 用法
 *
 *   API_SOLVER_KEY=xxx node tools/api_solver/server.mjs
 *   # 然后网关 config.json: "solver_urls": ["http://127.0.0.1:8001","http://127.0.0.1:8002"]
 *
 * ## 说明（如实）
 *
 * - 这是**第三方付费服务**的适配器；不配置 key 时服务照常启动但求解会返回 503。
 * - 本文件不含任何 key 硬编码（从环境变量读取）。
 * - 未联网/未配 key 时，单元测试仍可用（用注入的 fetch 桩）。
 */

import http from 'node:http';
import crypto from 'node:crypto';

const PROVIDER = (process.env.API_SOLVER_PROVIDER || 'capsolver').toLowerCase();
const KEY = process.env.API_SOLVER_KEY || '';
const PORT = parseInt(process.env.API_SOLVER_PORT || '8002', 10);
const POLL_MS = parseInt(process.env.API_SOLVER_POLL_MS || '3000', 10);
const TIMEOUT_MS = parseInt(process.env.API_SOLVER_TIMEOUT_MS || '120000', 10);

// ── 任务表（内存）──────────────────────────────────────
// task_id(本地) → { providerTaskId, createdAt, token?, error? }
const tasks = new Map();

/** 清理超过 timeout 的陈旧任务，避免内存无界增长。 */
export function gcTasks(now = Date.now()) {
  for (const [id, t] of tasks) {
    if (now - t.createdAt > TIMEOUT_MS * 2) tasks.delete(id);
  }
}

// ── 各 provider 的 API 适配 ─────────────────────────────
//
// 统一抽象：
//   createTask(sitekey, url, action) -> providerTaskId
//   getTask(providerTaskId)          -> { status:"process" } | { status:"success", value } | { status:"error", message }

/** capsolver: POST /createTask {clientKey, task:{type:"AntiTurnstileTaskProxyLess",websiteURL,websiteKey,metadata:{action}}} */
export const capsolver = {
  name: 'capsolver',
  createBody: (sitekey, url, action) => ({
    clientKey: KEY,
    task: {
      type: 'AntiTurnstileTaskProxyLess',
      websiteURL: url,
      websiteKey: sitekey,
      ...(action ? { metadata: { action } } : {}),
    },
  }),
  createUrl: 'https://api.capsolver.com/createTask',
  resultUrl: 'https://api.capsolver.com/getTaskResult',
  resultBody: (id) => ({ clientKey: KEY, taskId: id }),
  parseCreate: (j) => {
    if (j.errorId) throw new Error(`capsolver: ${j.errorCode || ''} ${j.errorDescription || ''}`.trim());
    if (!j.taskId) throw new Error('capsolver: 无 taskId');
    return j.taskId;
  },
  parseResult: (j) => {
    if (j.errorId) throw new Error(`capsolver: ${j.errorCode || ''} ${j.errorDescription || ''}`.trim());
    if (j.status === 'ready') {
      const v = j.solution?.token || j.solution?.gRecaptchaResponse;
      if (!v) throw new Error('capsolver: ready 但无 token');
      return { status: 'success', value: v };
    }
    return { status: 'process' };
  },
};

/** 2captcha: POST /in.php {key, method:"turnstile", sitekey, pageurl, action} → {taskId} ; GET /res.php?action=get&id= */
export const twocaptcha = {
  name: '2captcha',
  createUrl: 'https://2captcha.com/in.php',
  resultUrl: 'https://2captcha.com/res.php',
  createBody: (sitekey, url, action) => ({
    key: KEY,
    method: 'turnstile',
    sitekey,
    pageurl: url,
    json: 1,
    ...(action ? { action } : {}),
  }),
  resultQuery: (id) => `key=${encodeURIComponent(KEY)}&action=get&id=${encodeURIComponent(id)}&json=1`,
  parseCreate: (j) => {
    if (j.status !== 1) throw new Error(`2captcha: ${j.request || 'create 失败'}`);
    return String(j.request);
  },
  parseResult: (j) => {
    if (j.status === 1) return { status: 'success', value: String(j.request) };
    if (j.request === 'CAPCHA_NOT_READY') return { status: 'process' };
    throw new Error(`2captcha: ${j.request || '未知错误'}`);
  },
};

export function providerOf(name) {
  if (name === '2captcha') return twocaptcha;
  return capsolver;
}

// ── 求解流程 ──────────────────────────────────────────

/**
 * 向 provider 提交任务，返回 providerTaskId。
 * 失败抛错（调用方转 503）。
 */
export async function submit(provider, fetchImpl, sitekey, url, action) {
  if (!KEY) throw new Error('未配置 API_SOLVER_KEY');
  const r = await fetchImpl(provider.createUrl, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(provider.createBody(sitekey, url, action)),
  });
  return provider.parseCreate(await r.json());
}

/**
 * 查询一次 provider 任务状态。
 */
export async function poll(provider, fetchImpl, providerTaskId) {
  let r;
  if (provider.resultQuery) {
    r = await fetchImpl(`${provider.resultUrl}?${provider.resultQuery(providerTaskId)}`);
  } else {
    r = await fetchImpl(provider.resultUrl, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(provider.resultBody(providerTaskId)),
    });
  }
  return provider.parseResult(await r.json());
}

// ── HTTP 服务 ─────────────────────────────────────────

/** 供测试注入：默认用全局 fetch。 */
export function createServer({ fetchImpl = fetch, now = () => Date.now() } = {}) {
  const provider = providerOf(PROVIDER);

  return http.createServer(async (req, res) => {
    const u = new URL(req.url, `http://127.0.0.1:${PORT}`);
    const send = (code, obj) => {
      res.writeHead(code, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(obj));
    };

    if (u.pathname === '/health') {
      return send(200, {
        status: 'ok',
        backend: provider.name,
        key_configured: !!KEY,
        pending: tasks.size,
      });
    }

    if (u.pathname === '/turnstile') {
      const url = u.searchParams.get('url');
      const sitekey = u.searchParams.get('sitekey');
      const action = u.searchParams.get('action') || '';
      if (!url || !sitekey) return send(400, { error: '缺少 url/sitekey' });
      if (!KEY) return send(503, { error: '未配置 API_SOLVER_KEY（第三方求解服务）' });
      try {
        const providerTaskId = await submit(provider, fetchImpl, sitekey, url, action);
        const taskId = crypto.randomUUID();
        tasks.set(taskId, { providerTaskId, createdAt: now(), token: null, error: null });
        gcTasks(now());
        return send(202, { task_id: taskId, status: 'accepted' });
      } catch (e) {
        return send(503, { error: String(e.message || e) });
      }
    }

    if (u.pathname === '/result') {
      const id = u.searchParams.get('id');
      const t = id && tasks.get(id);
      if (!t) return send(404, { error: '未知/过期任务' });
      if (t.token) return send(200, { status: 'success', value: t.token });
      if (t.error) return send(200, { status: 'error', message: t.error });
      if (now() - t.createdAt > TIMEOUT_MS) {
        t.error = '求解超时';
        return send(200, { status: 'error', message: t.error });
      }
      try {
        const r = await poll(provider, fetchImpl, t.providerTaskId);
        if (r.status === 'success') {
          t.token = r.value;
          return send(200, { status: 'success', value: t.token });
        }
        return send(200, { status: 'process' });
      } catch (e) {
        t.error = String(e.message || e);
        return send(200, { status: 'error', message: t.error });
      }
    }

    return send(404, { error: 'not found' });
  });
}

// 仅在被直接执行时启动（被 import 时不启动，便于测试）
if (import.meta.url === `file://${process.argv[1]?.replace(/\\/g, '/')}`) {
  createServer().listen(PORT, '0.0.0.0', () => {
    console.log(`[api_solver] ${PROVIDER} 适配器监听 0.0.0.0:${PORT}（key ${KEY ? '已配置' : '未配置'}）`);
    if (!KEY) console.log('[api_solver] 警告：未设置 API_SOLVER_KEY，/turnstile 将返回 503');
  });
}
