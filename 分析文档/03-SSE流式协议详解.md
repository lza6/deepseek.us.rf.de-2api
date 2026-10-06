# 03 · SSE 流式协议详解（核心）

> 本文是构建 2API 代理的**最关键依据**。所有内容来自 `public-main.bundle.js` 的
> `aipkit_chatUI_createEventSource` / `aipkit_chatUI_streamMessage` / 各 `handle*Event` 函数。

---

## 1. 两步调用模型

AIPKit 的流式聊天**不是单次请求**，而是「先缓存消息 → 再建 SSE 流」两步：

```
┌─────────────────────────────────────────────────────────────────┐
│ 步骤 1：POST 缓存消息，换取 cache_key                              │
├─────────────────────────────────────────────────────────────────┤
│ POST /wp-admin/admin-ajax.php                                     │
│ Content-Type: application/x-www-form-urlencoded  (FormData)       │
│                                                                   │
│ action=aipkit_cache_sse_message                                   │
│ message=<用户消息文本>                                             │
│ _ajax_nonce=<nonce>                                               │
│ bot_id=<机器人ID>                                                  │
│ image_inputs=<JSON数组字符串>          (可选，图片输入)              │
│ user_client_message_id=<客户端消息ID>  (可选)                       │
│ [active_openai_vs_id / active_pinecone_* / active_qdrant_* /      │
│  active_chroma_* / active_claude_file_id]  (可选，向量库上下文)     │
│                                                                   │
│ ← 200 { success:true, data:{ cache_key:"<键>" } }                 │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌─────────────────────────────────────────────────────────────────┐
│ 步骤 2：建立 SSE 流（GET），服务端按 cache_key 取回消息并转发给 LLM    │
├─────────────────────────────────────────────────────────────────┤
│ GET /wp-admin/admin-ajax.php?action=aipkit_frontend_chat_stream   │
│     &cache_key=<步骤1返回>                                         │
│     &bot_id=<机器人ID>                                             │
│     &session_id=<访客UUID>                                         │
│     &conversation_uuid=<会话UUID>                                  │
│     &post_id=<文章ID>              (仅 >0 时)                       │
│     &_ts=<毫秒时间戳>              (缓存破坏)                        │
│     &_ajax_nonce=<nonce>                                          │
│     [+ 条件参数，见 §3]                                            │
│                                                                   │
│ ← 200 Content-Type: text/event-stream                             │
│   SSE 事件流（见 §5）                                              │
└─────────────────────────────────────────────────────────────────┘
```

**为什么这样设计**：用户消息可能很长且含敏感内容，放 GET query 会写入服务器访问日志。AIPKit 改为 POST 缓存换取短 `cache_key`，SSE 只传 key。

---

## 2. 步骤 1 —— `aipkit_cache_sse_message` 完整字段

代码位置：`public-main.bundle.js` 中 `window.aipkit_chatUI_cacheSseMessage`。

```js
const s = new FormData();
s.append("action", "aipkit_cache_sse_message");
s.append("message", userText);                    // 用户消息
s.append("_ajax_nonce", cfg.nonce);              // 必填
if (cfg.botId) s.append("bot_id", cfg.botId);    // 机器人 ID
if (imageDataPayload)                            // 图片输入（可选）
    s.append("image_inputs", JSON.stringify([imageDataPayload]));
if (clientUserMessageId)                         // 客户端消息 ID（可选）
    s.append("user_client_message_id", clientUserMessageId);

// 向量库文件上下文（可选，从 localStorage 恢复或参数传入）
if (ctx.provider === "OpenAI"  && ctx.vector_store_id)
    s.append("active_openai_vs_id", ctx.vector_store_id);
if (ctx.provider === "Pinecone" && ctx.index_name && ctx.namespace) {
    s.append("active_pinecone_index_name", ctx.index_name);
    s.append("active_pinecone_namespace", ctx.namespace);
}
if (ctx.provider === "Qdrant" && ctx.collection_name && ctx.file_upload_context_id) {
    s.append("active_qdrant_collection_name", ctx.collection_name);
    s.append("active_qdrant_file_upload_context_id", ctx.file_upload_context_id);
}
if (ctx.provider === "Chroma" && ctx.collection_name && ctx.file_upload_context_id) {
    s.append("active_chroma_collection_name", ctx.collection_name);
    s.append("active_chroma_file_upload_context_id", ctx.file_upload_context_id);
}
if (ctx.provider === "Claude" && ctx.file_id)
    s.append("active_claude_file_id", ctx.file_id);
```

**响应**（成功）：`{ success: true, data: { cache_key: "<string>" } }`

**403 处理**：若返回 403，前端会先用 `aipkit_get_frontend_chat_nonce` 刷新 nonce，再重试一次（见 `aipkit-public-main-js-after`）。

---

## 3. 步骤 2 —— 参数全清单

代码位置：`window.aipkit_chatUI_createEventSource`。参数按条件逐步 `append`：

| 参数 | 条件 | 值 | 说明 |
|------|------|----|----|
| `action` | 恒有 | `aipkit_frontend_chat_stream` | 路由 |
| `cache_key` | 恒有 | 步骤 1 返回 | 消息索引 |
| `bot_id` | 恒有 | 机器人 ID | |
| `session_id` | 恒有 | 访客 UUID 或 `""` | 身份 |
| `conversation_uuid` | 恒有 | 会话 UUID | 会话隔离 |
| `post_id` | `n > 0` | 文章 ID | WordPress 上下文 |
| `_ts` | try | `Date.now()` | 缓存破坏 |
| `previous_openai_response_id` | provider=OpenAI 且 `enableOpenAIConversationState` 且非空 | OpenAI 响应 ID | 多轮上下文 |
| `frontend_web_search_active` | `webSearchActive` 且 `allowWebSearchTool` 且 provider∈(OpenAI,Claude,OpenRouter,xAI) | `"true"` | 联网搜索开关 |
| `frontend_google_search_grounding_active` | `groundingActive` 且 provider=Google 且 `allowGoogleSearchGrounding` | `"true"` | Google 搜索接地 |
| `active_openai_vs_id` | 文件上下文 provider=OpenAI | vector_store_id | 向量库 |
| `active_pinecone_index_name` / `active_pinecone_namespace` | provider=Pinecone | | 向量库 |
| `active_qdrant_collection_name` / `active_qdrant_file_upload_context_id` | provider=Qdrant | | 向量库 |
| `active_chroma_collection_name` / `active_chroma_file_upload_context_id` | provider=Chroma | | 向量库 |
| `active_claude_file_id` | provider=Claude | | 文件 |
| `_ajax_nonce` | 恒有（最后 append） | nonce | 鉴权 |

**客户端实现**：`new EventSource(url)`（浏览器原生）。

---

## 4. 客户端状态机

流式引擎维护一个 `streamState` 对象：

```js
{
  dataReceived: false,              // 是否收到过数据
  currentStreamMessageId: null,     // 当前流消息 ID（由 message_start 设置）
  accumulatedGroundingMetadata: null,
  accumulatedCitations: [],         // 累积的引用
  currentStatusText: null,
  ignoreNextStreamError: false,     // 忽略下一个错误（主动 abort 时）
  ignoredStreamErrorRef: null,
}
```

事件监听注册：

```js
es.addEventListener("message_start",       e => handleMessageStartEvent(e, cfg, state, msgs));
es.addEventListener("openai_response_id",  e => handleOpenAIResponseIdEvent(e, cfg));
es.addEventListener("grounding_metadata",  e => handleGroundingMetadataEvent(e, state));
es.addEventListener("status",              e => handleStatusEvent(e, botId, cfg, msgs, state));
es.addEventListener("citations",           e => handleCitationsEvent(e, botId, cfg, msgs, state));
es.addEventListener("display_form_event",  e => handleDisplayFormEvent(e, botId, cfg, msgs, state));
es.onmessage =                               e => { handleOnMessageEvent(e, botId, cfg, msgs, state); sync(); };
es.addEventListener("done",                e => { sync(); handleDoneEvent(e, ...); state.ignoreNextStreamError = false; });
es.addEventListener("warning",             e => handleWarningEvent(e, botId, cfg, msgs, state));
es.onerror =                               e => { /* 见 §5.9 */ };
```

---

## 5. SSE 事件类型与数据帧

### 5.1 `message_start` （命名事件）— ✅ E2E 已验证

**作用**：声明一条新的助手消息开始，设置消息 ID。

**真实帧（E2E 抓包原文）**：
```
event: message_start
data: {"message_id":"aipkit-msg-6ac45e8eb8c5a6.44139757"}
```

```js
// 前端解析
const data = JSON.parse(event.data);
if (data.message_id) {
    state.currentStreamMessageId = data.message_id;
}
// 若 provider=OpenAI 且 enableOpenAIConversationState：
if (data.openai_response_id) {
    window.aipkit_current_openai_response_id = data.openai_response_id;
    sessionStorage.setItem("aipkit_current_openai_response_id", data.openai_response_id);
}
```

### 5.2 默认消息 `onmessage` —— 承载 `delta` — ✅ E2E 已验证

**作用**：逐块传输助手回复文本。

**真实帧（E2E 抓包原文，注意：无 `event:` 行 = SSE 默认 `message` 事件）**：
```
data: {"delta":""}

data: {"delta":"¡"}

data: {"delta":"Cl"}

data: {"delta":"aro! Aquí va"}
```

> ⚠️ **关键**：delta 帧**没有 `event:` 行**。SSE 以**空行**分隔事件；无 `event:` 字段的块
> 使用默认事件类型 `message`（对应浏览器 `EventSource.onmessage`）。解析时必须按空行分块，
> **不能沿用上一个 `event:` 名**（否则会把 delta 误标为 `message_start`）。

```js
// 前端解析
const data = JSON.parse(event.data);
const delta = data.delta;
if (delta !== undefined && state.currentStreamMessageId) {
    if (!state.dataReceived) {
        state.dataReceived = true;
        state.currentStatusText = null;
        // 把 typing indicator 转成正式消息气泡，写入内容
    } else {
        // 追加 delta 到当前消息气泡
    }
}
```

**首个 delta 可能为空串**（E2E 观察），需容忍。

### 5.3 `openai_response_id` （命名事件）

**作用**：OpenAI 会话状态追踪。

```js
const data = JSON.parse(event.data);
if (cfg.provider === "OpenAI" && cfg.enableOpenAIConversationState && data.id) {
    window.aipkit_current_openai_response_id = data.id;
    sessionStorage.setItem("aipkit_current_openai_response_id", data.id);
}
```

**推测数据帧**：`{ "id": "resp_xxx" }`

### 5.4 `grounding_metadata` （命名事件）

**作用**：Google Search Grounding 元数据。

```js
const data = JSON.parse(event.data);
state.accumulatedGroundingMetadata = data;
```

**推测数据帧**：Google grounding metadata 对象。

### 5.5 `status` （命名事件）

**作用**：显示处理状态（如 "Searching web..."、"Calling tool..."）。

```js
// handleStatusEvent → 渲染为 typing indicator 的状态文本
// 状态判定逻辑：
function isEmptyStatus(text, textCfg) {
    const list = [textCfg.statusProcessing, "processing", "processing...", "streaming", "streaming..."];
    return list.map(s=>s.trim().toLowerCase()).includes(text.trim().toLowerCase());
}
// 非空状态文本优先；否则按 type/content_block_type/name 推断：
//   web_search  → textCfg.statusSearchingWeb || "Searching web..."
//   file_search → textCfg.statusRetrievingContext
//   tool/function_call/image_generation_call/tool_use/server_tool_use → textCfg.statusCallingTool || "Calling tool..."
```

**推测数据帧**：`{ "text": "Searching web..." }` 或 `{ "type": "web_search", "content_block_type": "tool_use", "name": "..." }`

### 5.6 `citations` （命名事件）

**作用**：来源引用列表。

```js
// handleCitationsEvent → 累积到 state.accumulatedCitations，并在 done 时渲染
// 归一化函数 normalizeCitations 会去重、规范 URL
```

**推测数据帧**：
```json
{ "citations": [
    { "title": "...", "url": "...", "excerpt": "...", "location": "..." }
] }
```

### 5.7 `display_form_event` （命名事件）

**作用**：动态表单（AIPKit 的表单构建器）。

```js
const data = JSON.parse(event.data);
const formDef = data.form_definition;
if (typeof formDef !== "object" || formDef === null) { console.error(...); return; }
window.aipkit_chatUI_renderChatForm(formDef, ...);
```

**推测数据帧**：
```json
{ "form_definition": {
    "form_id": "...",
    "title": "...",
    "elements": [
      { "type": "text_input|textarea|dropdown|radio_group|checkbox_group|heading|label", ... }
    ]
} }
```
表单元素类型（从 `renderChatForm` 的 case 分支）：`text_input`、`textarea`、`dropdown`、`radio_group`、`checkbox_group`、`heading`、`label`。

### 5.8 `warning` （命名事件）

**作用**：非致命警告。

```js
const data = JSON.parse(event.data);
if (data.error && state.currentStreamMessageId) {
    // 把错误文本以斜体追加到消息末尾
    appendOrUpdateMessage(msgs, state.currentStreamMessageId, `  *${data.error}*`, "bot", ...);
}
```

**推测数据帧**：`{ "error": "警告文本" }`

### 5.9 `done` （命名事件）— ✅ E2E 已验证

**作用**：流结束。

**真实帧（E2E 抓包原文）**：
```
event: done
data: {"finished":true}
```

```js
const data = JSON.parse(event.data);
if (!state.accumulatedGroundingMetadata && data.grounding_metadata)
    state.accumulatedGroundingMetadata = data.grounding_metadata;
if (Array.isArray(data.citations) && data.citations.length) {
    state.accumulatedCitations = normalizeCitations(
        (state.accumulatedCitations || []).concat(data.citations));
}
// 用累积数据最终更新消息（含引用与接地）
appendOrUpdateMessage(msgs, state.currentStreamMessageId, "", "bot", cfg,
    true, false, state.accumulatedGroundingMetadata, state.accumulatedCitations);
es.close();   // 关闭流
```

**推测数据帧**：`{ "citations": [...], "grounding_metadata": {...} }`（均可省略）

### 5.10 `error` —— 通过 `onerror` / 命名事件 — ✅ E2E 已验证

**真实帧（未过安全校验时，E2E 抓包原文）**：
```
event: error
id: err-1791252865
data: {"error":"Sicherheitspruefung erforderlich.","ts_required":true}

event: done
data: {"finished":true}
```

**作用**：错误 / 配额耗尽 / 安全校验要求。

```js
// handleErrorEvent 解析
const data = event.data ? JSON.parse(event.data) : null;
let errMsg = null, quotaNotice = null;
if (data) {
    if (data.error) errMsg = data.error;
    if (data.quota_notice) quotaNotice = data.quota_notice;
} else if (event.target?.readyState === EventSource.CLOSED) {
    errMsg = "Connection was closed.";
} else if (event.message) {
    errMsg = event.message;
}
// 构造并分发错误
callback(quotaNotice
    ? { type:"quota_notice", notice:quotaNotice, message: errMsg || quotaNotice.message }
    : errMsg, false, true);
es.close();
```

**配额通知结构**（`quota_notice`）：
```json
{ "quota_notice": {
    "title": "配额标题",
    "message": "配额消息（可含 HTML，如 <a href=\"#dsgt-buy-tokens\">）",
    "actions": [
      { "label": "购买 tokens", "url": "https://...", "variant": "primary|secondary" }
    ]
} }
```

> `buy-tokens.js` 专门监听 `aipkit:messageError` 事件的 `quota_notice`，自动弹出购买弹窗。

### 5.11 事件时序图

```
SSE 连接建立
    │
    ├─ message_start      → 设置 currentStreamMessageId，开始渲染
    │
    ├─ openai_response_id → (仅 OpenAI) 记录响应 ID
    ├─ grounding_metadata → (可选) 累积接地元数据
    ├─ status             → (可多次) 显示 "Searching web..." 等
    │
    ├─ message (delta)    → (多次) 逐块追加文本           ← 主要数据通道
    ├─ message (delta)    → ...
    │
    ├─ citations          → (可选) 累积引用
    ├─ display_form_event → (可选) 中途要求用户填表单
    ├─ warning            → (可选) 警告
    │
    └─ done               → 固化 + 渲染引用 → es.close()
       │ (或)
       └─ error            → 错误/配额通知 → es.close()
```

---

## 6. 与 OpenAI 格式的映射（给 2API 用）

| AIPKit | OpenAI `/v1/chat/completions` (stream) |
|--------|----------------------------------------|
| `message_start.message_id` | 生成 `id`（`chatcmpl-xxx`） |
| `message.delta` | `choices[0].delta.content` |
| `done` | `data: [DONE]` |
| `status` | 忽略（或映射为自定义字段） |
| `citations` | 忽略（或附加到 `content` 尾部） |
| `grounding_metadata` | 忽略（或映射为 `annotations`） |
| `error.quota_notice` | 抛 HTTP 429 / 自定义错误 |
| `warning` | 忽略（或日志） |
| `display_form_event` | 忽略（非对话内容） |

**翻译要点**：
1. 上游每个 `delta` → 一个 `chat.completion.chunk`，`finish_reason` 为 null
2. 上游 `done` → `finish_reason: "stop"` 的 chunk + `[DONE]`
3. 拿到 `cache_key` 后立刻建 SSE，用 `event: ...` 和默认 `message` 双重解析

---

## 7. 已实测确认项（E2E）

| 项 | 结果 |
|----|------|
| `delta` 帧字段名 | ✅ `{"delta":"<文本>"}` |
| `done` 是否带 body | ✅ `{"finished":true}` |
| 事件间是否有 `id:` 行 | 仅 `error` 事件有 `id: err-<num>`；正常流无 |
| `message_start` 帧 | ✅ `{"message_id":"aipkit-msg-<hex>.<hex>"}` |
| SSE 分隔单位 | ✅ 空行（`\n\n`） |
| delta 事件名 | ✅ 无 `event:` 行 = 默认 `message` 事件 |
| `error`/安全校验帧 | ✅ `{"error":"Sicherheitspruefung erforderlich.","ts_required":true}` |
| `cache_key` 是否一次性 | ✅ 消费即失效，复用报 `Message not found in cache.` |
| Turnstile 是否必需 | ✅ 必需（无 cookie 时被 `ts_required` 拦截） |

### 7.1 仍未触发的项

| 项 | 说明 |
|----|------|
| `status` 帧实际结构 | 本站（provider=DeepSeek，无工具）未触发 |
| `citations` / `grounding_metadata` 帧 | 本站未触发（无联网/接地） |
| `display_form_event` 帧 | 本站无表单功能 |
| 服务端是否校验 `Origin`/`Referer` | 未单独测试（代理侧建议复制浏览器头） |
