---
name: add-endpoint
description: 给 deepseek-es-2api 网关新增 API 端点/协议能力时的完整工作流。当用户要求「新增端点」「接入新功能」「加个 API」时使用。覆盖：先读记忆→判断过时→编码→全链路验证→文档同步。
---

# 新增端点 / 功能的完整工作流

> 本项目是 **Rust API 网关**（deepseek.es → OpenAI/Anthropic 兼容）。上游协议**非标准**，
> 所有改动必须**真实 E2E 验证**，禁止伪造。

## 第 0 步：读取上下文（强制，先于任何编码）

按顺序读，**不跳过**：

1. `VERIFICATION_LOG.md` —— 判断目标文件/模块**是否已优化过**（若未改动，跳过重复优化）
2. `计划书/下一步改进指南.md` —— 查是否已列入路线图、有无已记录的结论
3. `.claude/projects/.../memory/deepseek-es-2api-project.md`（若存在）—— 项目记忆
4. `分析文档/00-总览与索引.md` —— 上游协议权威来源

**判断过时**：若记忆/文档提到的 `file:line` 或函数名在代码中已不存在 → 标注过时并更新它，再继续。

## 第 1 步：先想清楚（禁止直接编码）

回答这些问题，写进 `<thinking>`：

- **这是「可落地」还是「上游未启用」？** 判断标准：能否产出**客户端可用的真实结果**。
  - 可落地（做）：如余额查询（上游有 REST 端点）、协议翻译（工具调用）
  - 不可落地（不做，如实标注）：上游 `allowXxx=false` 的能力 —— 转发参数上游也不响应，实现即伪造
- **两套协议（OpenAI + Anthropic）都要支持吗？** 默认都要，否则调用方会踩坑。
- **流式 + 非流式都要支持吗？** 默认都要。
- **输入校验**：畸形请求体、缺字段、类型错误 → 必须返回**对应协议**的错误结构（见下「契约铁则」）。

## 第 2 步：契约铁则（最易踩坑）

1. **请求体解析失败必须返回协议原生错误结构**：
   - 不要直接用 `Json<T>` 提取器（反序列化失败会返回 **422 纯文本**，SDK 解析失败）
   - 用 `axum::body::Bytes` + `serde_json::from_slice`，失败时返回 `AppError::BadRequest(..).into_response()`（OpenAI）
     或 `.into_anthropic_response()`（Anthropic）
   - 参考 `src/api.rs` 的 `openai_chat` / `anthropic_messages`
2. **错误体格式分协议**：OpenAI `{"error":{...}}`；Anthropic `{"type":"error","error":{...}}`
3. **未知字段要容忍**（不要 `#[serde(deny_unknown_fields)]`）—— 真实客户端会发额外字段
4. **所有端点都要 `check_auth`**（除非刻意公开如 `/healthz`）

## 第 3 步：TDD 实现

1. **先写失败测试**（单元 + 集成）
2. 实现，位置遵循现有分层：
   - 上游调用 → `src/upstream.rs`
   - 协议翻译 → `src/protocol/{openai,anthropic}.rs`
   - HTTP 路由/处理器 → `src/api.rs`
   - 配置 → `src/config.rs`（记得同步 `config.example.json`）
3. 跑定向测试 → 全量 `cargo test --all`

## 第 4 步：全链路验证（禁止只跑单测）

```bash
cargo test --all                                    # 全量（当前基线 135+46）
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo build --release

# 真实 E2E（需 cf_solver 在 127.0.0.1:8001）
node scripts/e2e-gateway.mjs "Di hola"    # 基础
node scripts/e2e-tools.mjs                # 工具调用
node scripts/e2e-meta.mjs                 # 余额/会话
node scripts/e2e-multiturn.mjs            # 多轮

# 压测（验证高并发无回归）
node scripts/loadtest.mjs 8 24
```

**反证（关键）**：对修复类改动，临时恢复旧实现跑新测试 → 确认旧实现**会失败**，证明修复解决的是真问题。

## 第 5 步：文档同步（强制）

- `README.md`：端点表、配置表、能力说明
- `CHANGELOG.md`：新增版本段（Keep a Changelog 格式）
- `config.example.json`：新配置项
- `Cargo.toml`：版本号
- `docs/PROTOCOL.md`：若涉及协议细节
- `VERIFICATION_LOG.md`：追加本次优化项 + 验证命令

## 第 6 步：提交

```bash
git add -A
git commit -F <msg-file>        # 用文件避免 shell 转义问题
git push origin main            # 仓库主分支是 main
git tag -a vX.Y.Z -m "..."
git push origin vX.Y.Z
"/c/Program Files/GitHub CLI/gh.exe" release create vX.Y.Z --title "..." --notes-file -
# 等 CI 绿
"/c/Program Files/GitHub CLI/gh.exe" run list --limit 1
```

## 验收门禁（Definition of Done）

- [ ] 单元 + 集成测试全绿（不退化基线）
- [ ] `cargo fmt --check` + `clippy -D warnings` 干净
- [ ] **真实 E2E 通过**（非仅 mock）
- [ ] 契约边界测试（畸形请求体 → 协议原生错误）
- [ ] 压测无回归
- [ ] 文档同步（README/CHANGELOG/VERIFICATION_LOG）
- [ ] 反证（修复类改动）
- [ ] CI 绿

## 禁止事项

- 禁止伪造上游没有的能力（如实标注「不适用」）
- 禁止把「有代码片段」当「已完成」（必须有生产调用点）
- 禁止只用 `Json<T>` 提取器（422 纯文本陷阱）
- 禁止跳过文档同步
- 禁止命令拼接用 shell heredoc（用文件 + `-F`）
