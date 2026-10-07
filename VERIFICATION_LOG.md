# VERIFICATION_LOG.md — 验证记录与已优化项

> **用途**：记录「已验证/已优化」的项，避免下次重复审查同一处。
> **AI 使用方式**：动手前先读本文件 → 判断是否已覆盖 → 只针对**改动过的区域**重跑对应验证。

## 如何判断是否需要重跑

1. 读「已优化项」表 → 找到你即将改动的文件/模块。
2. 若该文件标注了「上次验证版本」且**版本未变** → **跳过**，无需重跑。
3. 若该文件**已改动**（版本或内容变化）→ 只重跑该行的「验证命令」。
4. 新增功能/端点 → 追加新行。

---

## 已优化项（勿重复优化）

| 模块/文件 | 优化内容 | 上次验证版本 | 验证命令 |
|-----------|---------|-------------|---------|
| `src/upstream.rs` `parse_sse_stream` | H1 字节级 UTF-8 缓冲 + M1 CRLF + 8MB 上限 | v0.4.0 | `cargo test --lib upstream::` |
| `src/protocol/{openai,anthropic}.rs` `messages_to_prompt` | H2 多轮历史渲染 | v0.4.0 | `cargo test --lib protocol::` |
| `src/errors.rs` | H3 `into_anthropic_response` | v0.5.0 | `cargo test --lib errors::` |
| `src/features.rs` | H4 `inject_prompt_prefixes` | v0.5.0 | `cargo test --lib features::` |
| `src/upstream.rs` 客户端构造 | M2 双客户端（AJAX + SSE read_timeout） | v0.6.0 | `cargo test --lib upstream::` |
| `src/upstream.rs` `cache_message` | M5 nonce 失效重试 | v0.6.0 | `cargo test --lib upstream::tests::m5` |
| `src/upstream.rs` `spawn_prefetch` | M8 后台预取 | v0.6.0 | `cargo test --lib upstream::tests::m8` |
| `src/upstream.rs` `breaker_*` | M9 半开单探测 | v0.6.0 | `cargo test --lib upstream::tests::breaker` |
| `src/api.rs` `start_stream` | M3 哨兵结构化 + M10 流式配额 429 | v0.8.0 | `cargo test --test gateway streaming_quota` |
| `src/tools.rs` | v2.0.0 协议级工具 + `StreamToolFilter` | v0.7.0 | `cargo test --lib tools::` |
| `src/upstream.rs` `fetch_balance`/`list_conversations` | v0.8.0 余额/会话端点 | v0.8.0 | `cargo test --test gateway balance_or_conversations` |

## 已知「不适用」项（勿浪费时间）

| 项 | 原因 |
|----|------|
| 图片/视频生成、付费 API 真实调用 | 预算 0 + 上游未启用 |
| UI/前端/按钮/页面审查 | 本项目是纯 API 网关，无前端 |
| Load Balancer / Redis / Kafka / Sharding / DB 复制 | 单二进制网关对接单体上游，过度设计 |
| 向量库 RAG / OpenAI 响应链 / 联网接地 | 上游 bot 未启用，实现即伪造 |

## 验证命令速查

```bash
cargo test --all                          # 全量（135 单测 + 42 集成）
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
node scripts/e2e-gateway.mjs "Di hola"    # 真实 E2E（需 cf_solver）
node scripts/e2e-tools.mjs                # 工具调用真实往返
node scripts/e2e-meta.mjs                 # 余额/会话端点
node scripts/e2e-multiturn.mjs            # 多轮历史
node scripts/loadtest.mjs 8 24            # 压测
```

## 变更历史

- 2026-10-07：建立本文件（v0.9.0 终局审计时）。

## 反思发现的真实缺陷（v0.10.0 修复）

> 由 `/reflexion:reflect` 自查发现——**我此前把 v0.8.0 的 E2E 结果当作 v0.9.0 的**，
> 且漏了两个真实工程缺陷。

| 缺陷 | 证据 | 修复 |
|------|------|------|
| **R1 假称 E2E**：v0.9.0 报告写"真实 E2E 5/5"，但 `e2e-meta-config.json` 时间戳 17:38（v0.8.0），`api.rs` 20:22 才改 | `ls -la` vs `git log` | 对最终二进制重跑 5/5 + 8/8 |
| **R2 413 纯文本**：body 超限返回 `Failed to buffer the request body`（纯文本，SDK 解析失败），且 axum 默认 2MB 对长上下文偏小 | 实测 `3MB → 413 纯文本` | 新增 `max_request_bytes`（默认 8MB） |
| **R3 账本无限增长**：`usage.db-wal` 2.9MB，无 retention/checkpoint | `ls` + grep 无 `DELETE FROM` | 新增 `prune()` + `wal_autocheckpoint` + 每 6h 后台清理 |

## 反思教训（务必遵守）

1. **E2E 必须针对最终二进制**：改完代码后重跑，不能沿用上一版结果。**先看时间戳**。
2. **同类 bug 要扫全**：修了"错误体不结构化"，就必须扫**所有**可能返回非结构化错误的地方
   （Body limit 413、限流中间件、并发限流 503）。
3. **声称前必须用命令核实**，不能用记忆。

