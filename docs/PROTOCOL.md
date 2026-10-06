# 上游协议（deepseek.es / AIPKit）

> 完整逆向分析见 `分析文档/`。本文件是面向开发的精简契约。

## 认证流程

```
1. Cloudflare Turnstile 求解
   GET <cf_solver>/turnstile?url=https://deepseek.es/&sitekey=0x4AAAAAADlLZ3ljqZP6cQwq&action=chat
   → {task_id} → 轮询 <cf_solver>/result?id= → {status:"success", value:"<token>"}

2. 兑换安全 Cookie
   POST <upstream>/wp-admin/admin-ajax.php
   action=deepseek_ts_verify&token=<token>
   → {"ok":true} + Set-Cookie: dsts_ok=1; dsts=<hash>

3. 获取 nonce
   POST action=aipkit_get_frontend_chat_nonce&bot_id=27623
   → {"success":true,"data":{"nonce":"..."}}
```

## 聊天流程（两步）

```
4. 缓存消息
   POST action=aipkit_cache_sse_message
        message=<text>&_ajax_nonce=<nonce>&bot_id=27623
   → {"success":true,"data":{"cache_key":"aipkit_sse_..."}}

5. SSE 流
   GET  ?action=aipkit_frontend_chat_stream
        &cache_key=<key>&bot_id=27623
        &session_id=<uuid>&conversation_uuid=<uuid>
        &_ajax_nonce=<nonce>&_ts=<ms>
   → text/event-stream
```

## SSE 事件（实测）

```
event: message_start
data: {"message_id":"aipkit-msg-<hex>.<hex>"}

data: {"delta":""}                      ← 无 event: 行 = 默认 message 事件
data: {"delta":"文本片段"}
...

event: done
data: {"finished":true}
```

**错误 / 安全校验**：
```
event: error
id: err-1791252865
data: {"error":"Sicherheitspruefung erforderlich.","ts_required":true}

event: done
data: {"finished":true}
```

**配额耗尽**：
```
event: error
data: {"error":"...","quota_notice":{"title":"...","message":"...","actions":[...]}}
```

## 关键约束

| 约束 | 说明 |
|------|------|
| SSE 分隔 | 空行（`\n\n`） |
| delta 事件名 | 默认 `message`（无 `event:` 行），**不能沿用上一个 event 名** |
| `cache_key` | **一次性**，消费即失效 |
| Turnstile | **必需**，无 `dsts_ok` cookie 时被 `ts_required` 拦截 |
| 首个 delta | 可能为空串 |
| 首帧填充 | 一个 `:` 注释行 + 大量空格（防缓冲），需忽略 |
| 多轮上下文 | 上游按 `conversation_uuid` 维护 |

## 会话/身份

| 标识 | 来源 | 用途 |
|------|------|------|
| `session_id` | 访客 UUID | 身份 |
| `conversation_uuid` | 客户端生成 | 会话隔离 + 多轮上下文 |

## 站点实测参数

| 项 | 值 |
|----|-----|
| bot_id | 27623 |
| provider | DeepSeek |
| sitekey | 0x4AAAAAADlLZ3ljqZP6cQwq |
| postId | 27568 |
