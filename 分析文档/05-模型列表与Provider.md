# 05 · 模型列表与 Provider

> **重要前提**：deepseek.es 的前端 **不暴露具体模型 ID**。前端只携带 `provider`，
> 真正的模型由服务端 bot 配置（WordPress 后台）决定。因此"模型列表端点"不存在于前端协议中。
>
> **E2E 实测（2026-10-06）**：本站 `data-config.provider = **"DeepSeek"**`，botId=27623。

---

## 0. 本站实测结论（E2E 真实数据）

| 项 | 实测值 |
|----|--------|
| `provider` | **`DeepSeek`** |
| `botId` | 27623 |
| `postId` | 27568 |
| `headerName` | "DeepSeek ES" |
| `footerText` | "Powered by DeepSeek API" |
| `allowWebSearchTool` | false |
| `allowGoogleSearchGrounding` | false |
| `enableSidebar` / `enableDownload` / `enableFeedback` | 全 false |
| `imageUploadEnabledUI` / `fileUploadEnabledUI` | false |
| `ttsEnabled` / `enableVoiceInputUI` / `enableRealtimeVoiceUI` | false |
| `enableStarters` | false |
| `showSources` | true |
| `imageTriggers` | `"/image, /generate"`（虽配置但上传关闭） |

**结论**：本站是**纯文本聊天**，provider=DeepSeek（AIPKit 内建枚举外的扩展），无工具/联网/上传。

---

## 1. Provider 支持矩阵

### 1.1 AIPKit bundle 枚举的 5 个 Provider

从 `public-main.bundle.js` 分支判断确认：

| Provider 值 | 厂商 | 前端识别 | Web Search | Google Grounding | 会话状态 | 文件上下文 |
|------------|------|---------|-----------|-----------------|---------|-----------|
| `OpenAI` | OpenAI | ✅ | ✅ | ❌ | ✅ (`enableOpenAIConversationState`) | vector_store_id |
| `Claude` | Anthropic | ✅ | ✅ | ❌ | ❌ | file_id |
| `Google` | Google Gemini | ✅ | ❌ | ✅ (`allowGoogleSearchGrounding`) | ❌ | ❌ |
| `OpenRouter` | OpenRouter 聚合 | ✅ | ✅ | ❌ | ❌ | ❌ |
| `xAI` | xAI Grok | ✅ | ✅ | ❌ | ❌ | ❌ |

> **`DeepSeek` 是 AIPKit 的隐藏/扩展 provider**（不在前端 5 个分支判断中，但服务端支持）。
> 本站实测 `provider="DeepSeek"`，说明站点用 DeepSeek 官方 API（或兼容端点）接入。

### 1.2 前端分支判定代码（Web Search / Grounding）
```js
const webSearchAvailable = cfg.allowWebSearchTool &&
    (cfg.provider === "OpenAI" || cfg.provider === "Claude" ||
     cfg.provider === "OpenRouter" || cfg.provider === "xAI");
```

**判定代码**（Google Grounding）：
```js
const groundingAvailable = cfg.allowGoogleSearchGrounding && cfg.provider === "Google";
```

---

## 2. 向量库（RAG）支持

文件上下文（`activeFileContext`）按 Provider 分派：

| Provider | 参数 | 说明 |
|----------|------|------|
| `OpenAI` | `active_openai_vs_id` | OpenAI Vector Store ID |
| `Pinecone` | `active_pinecone_index_name` + `active_pinecone_namespace` | Pinecone |
| `Qdrant` | `active_qdrant_collection_name` + `active_qdrant_file_upload_context_id` | Qdrant |
| `Chroma` | `active_chroma_collection_name` + `active_chroma_file_upload_context_id` | Chroma |
| `Claude` | `active_claude_file_id` | Claude 文件 |

> 注意：Pinecone/Qdrant/Chroma 是**向量库 Provider**，与 LLM Provider 正交。文件上下文结构：
> ```json
> { "provider": "Qdrant", "collection_name": "...", "file_upload_context_id": "..." }
> ```

---

## 3. "模型列表"的真实来源

由于前端不含模型名，2API 代理要获得"模型列表"，只能：

### 方案 A：静态映射（推荐起步）
把 `provider` 映射为展示名，暴露给下游：

```
deepseek-es            → 默认（服务端配置的模型）
deepseek-es-openai     → provider=OpenAI
deepseek-es-claude     → provider=Claude
deepseek-es-google     → provider=Google
deepseek-es-openrouter → provider=OpenRouter
deepseek-es-xai        → provider=xAI
```

### 方案 B：探测式
对每个 provider 发一条最小消息，从 SSE `message_start` / 响应内容特征反推实际模型。
（成本高、不可靠，不推荐）

### 方案 C：抓取 WordPress 后台/公开配置
若站点暴露 bot 列表 API（如 `wp-json/aipkit/v1/...`），可读取 bot → provider/model 映射。
**待探测**：`/wp-json/` 根路由是否列出 AIPKit 命名空间。

---

## 4. AIPKit 后端可能支持的真实模型（厂商侧）

以下是各 Provider 在 2025-2026 年常见的模型（**非本站确认，仅供参考**）：

| Provider | 常见模型 |
|----------|---------|
| OpenAI | gpt-4o, gpt-4o-mini, gpt-4.1, o1, o3, o4-mini |
| Claude | claude-sonnet-4-5, claude-opus-4, claude-haiku |
| Google | gemini-2.5-pro, gemini-2.5-flash, gemini-2.0-flash |
| OpenRouter | 聚合（按站点配置） |
| xAI | grok-3, grok-4 |

> ⚠️ **deepseek.es 的名字暗示可能用 DeepSeek 模型**，但前端 Provider 枚举中**没有 `DeepSeek`**——
> 说明 DeepSeek 模型可能是通过 `OpenAI` 兼容端点（provider=OpenAI + 自定义 base_url）接入的，
> 或通过 `OpenRouter` 接入。**这是 E2E 必须验证的关键点。**

---

## 5. 模型能力探测清单（E2E）

| 待验证 | 方法 | 结果 |
|--------|------|------|
| 默认 bot 的 provider | 抓聊天页 `data-config.provider` | ✅ **`DeepSeek`** |
| 默认模型名 | 发消息后看响应风格 / 问"你是谁" | 待验证（疑 deepseek-chat） |
| 是否支持联网 | `allowWebSearchTool` | ✅ false |
| 是否支持 Google 接地 | `allowGoogleSearchGrounding` | ✅ false |
| 是否多 bot | 页面 `.aipkit_chat_container` 数量 | 1 个（botId=27623） |
| 是否有模型切换 UI | `dsbx-model-badge` | 无关（本站 enableSidebar=false） |
| 输出语言 | 实测 | 西语（站点默认 locale=es） |
| 输出能力 | 实测 | 高质量 Markdown（代码块/表格/引用） |

---

## 6. `dsbx_get_bot_model` 端点

自研层提供了一个**模型展示名**端点：

```
GET /wp-admin/admin-ajax.php?action=dsbx_get_bot_model&bot_id=<id>
→ { success: true, data: { label: "<模型展示名>" } }
```

这是**唯一**能从前端拿到"模型名"的接口（且只是展示标签，非真实模型 ID）。
`deepseek-chat-fullscreen-sidebar-js-after` 用它给聊天头注入 `dsbx-model-badge`。

> 2API 代理可用它作为 `/v1/models` 的**展示名来源**，配合 `provider` 拼装模型列表。

---

## 7. 结论

- **无标准"模型列表端点"**：模型由服务端 bot 配置决定
- **可获取**：`provider`（前端）、展示名（`dsbx_get_bot_model`）
- **不可获取**：真实模型 ID、采样参数（temperature 等前端不传）
- **2API 策略**：暴露 provider 级别的模型别名 + 展示名，或单模型透传
