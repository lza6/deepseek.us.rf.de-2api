//! 上游 deepseek.es 客户端：Turnstile 认证 → nonce → cache_sse_message → SSE 流。
//!
//! 全流程（E2E 验证）：
//!   1. (首次/失效) solve Turnstile → POST admin-ajax `deepseek_ts_verify&token=` → dsts_ok cookie
//!   2. POST `aipkit_get_frontend_chat_nonce&bot_id=` → nonce
//!   3. POST `aipkit_cache_sse_message&message=&_ajax_nonce=&bot_id=` → cache_key（一次性）
//!   4. GET  `aipkit_frontend_chat_stream&cache_key=&bot_id=&session_id=&conversation_uuid=&_ajax_nonce=` → SSE
//!
//! SSE 协议（真实抓包确认）：
//!   event: message_start        data: {"message_id":"..."}
//!   (默认 message 事件)          data: {"delta":"..."}
//!   event: done                 data: {"finished":true}
//!   event: error                data: {"error":"...","ts_required":true}

use crate::config::Config;
use crate::errors::{AppError, AppResult};
use crate::solver::{urlencode, SolverClient};
use futures::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// 上游客户端（携带 cookie jar + 缓存的 nonce/cookie）。
pub struct UpstreamClient {
    cfg: Config,
    http: reqwest::Client,
    solver: Arc<SolverClient>,
    /// 缓存的安全 cookie 状态
    state: Mutex<AuthState>,
}

#[derive(Debug, Default, Clone)]
struct AuthState {
    /// cookie 名→值
    cookies: Vec<(String, String)>,
    /// dsts_ok cookie 获取时刻
    cookie_obtained: Option<Instant>,
    /// 当前 nonce
    nonce: Option<String>,
    /// bot_id（来自配置，但可被 data-config 覆盖）
    bot_id: String,
    /// 认证过程串行化锁（防止并发触发多次求解）
    solving: bool,
}

fn decode_entities(s: &str) -> String {
    s.replace("&#038;", "&")
        .replace("&amp;", "&")
        .replace("&#039;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

impl UpstreamClient {
    pub fn new(cfg: Config) -> AppResult<Self> {
        let mut builder = reqwest::Client::builder()
            .cookie_store(false) // 手动管理 cookie
            .timeout(Duration::from_secs(cfg.http_timeout_secs));
        if let Some(p) = &cfg.proxy {
            builder = builder.proxy(
                reqwest::Proxy::all(p)
                    .map_err(|e| AppError::Internal(format!("代理配置无效: {e}")))?,
            );
        }
        let http = builder
            .build()
            .map_err(|e| AppError::Internal(format!("HTTP 客户端构建失败: {e}")))?;
        let solver = Arc::new(SolverClient::new(&cfg)?);
        Ok(UpstreamClient {
            state: Mutex::new(AuthState {
                bot_id: cfg.bot_id.clone(),
                ..Default::default()
            }),
            cfg,
            http,
            solver,
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    fn ajax_url(&self) -> String {
        self.cfg.ajax_url()
    }

    fn cookie_header(state: &AuthState) -> Option<String> {
        if state.cookies.is_empty() {
            None
        } else {
            Some(
                state
                    .cookies
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        }
    }

    fn base_headers(&self, state: &AuthState) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
        let mut h = HeaderMap::new();
        let ua = HeaderValue::from_str(&self.cfg.user_agent)
            .unwrap_or(HeaderValue::from_static("Mozilla/5.0"));
        h.insert(reqwest::header::USER_AGENT, ua);
        let base = self.cfg.upstream_base_url.trim_end_matches('/');
        h.insert(
            reqwest::header::ORIGIN,
            HeaderValue::from_str(base).unwrap_or(HeaderValue::from_static("*")),
        );
        h.insert(
            reqwest::header::REFERER,
            HeaderValue::from_str(&format!("{base}/")).unwrap_or(HeaderValue::from_static("*")),
        );
        h.insert(
            reqwest::header::ACCEPT_LANGUAGE,
            HeaderValue::from_static("es-ES,es;q=0.9,en;q=0.8"),
        );
        if let Some(c) = Self::cookie_header(state) {
            if let Ok(v) = HeaderValue::from_str(&c) {
                h.insert(reqwest::header::COOKIE, v);
            }
        }
        let _ = HeaderName::from_static("x-requested-with");
        h
    }

    fn absorb_set_cookie(state: &mut AuthState, headers: &reqwest::header::HeaderMap) {
        for v in headers.get_all(reqwest::header::SET_COOKIE).iter() {
            if let Ok(s) = v.to_str() {
                if let Some(pair) = s.split(';').next() {
                    if let Some(idx) = pair.find('=') {
                        let k = pair[..idx].trim().to_string();
                        let val = pair[idx + 1..].trim().to_string();
                        if let Some(slot) = state.cookies.iter_mut().find(|(ck, _)| ck == &k) {
                            slot.1 = val;
                        } else {
                            state.cookies.push((k, val));
                        }
                    }
                }
            }
        }
    }

    fn has_ok_cookie(state: &AuthState) -> bool {
        state
            .cookies
            .iter()
            .any(|(k, v)| k == "dsts_ok" && v == "1")
    }

    /// 从页面 HTML 提取 data-config（含 botId/nonce/provider）。
    pub async fn fetch_page_config(&self) -> AppResult<PageConfig> {
        let url = format!("{}/", self.cfg.upstream_base_url.trim_end_matches('/'));
        let r = self
            .http
            .get(&url)
            .headers(self.base_headers(&AuthState::default()))
            .send()
            .await
            .map_err(|e| AppError::Network(format!("抓取首页失败: {e}")))?;
        let html = r
            .text()
            .await
            .map_err(|e| AppError::Network(format!("读取首页失败: {e}")))?;
        parse_page_config(&html)
    }

    /// 确保已认证（有 dsts_ok cookie 且未过期）。采用双检锁避免并发重复求解。
    pub async fn ensure_authed(&self) -> AppResult<()> {
        // 快速路径
        {
            let st = self.state.lock().await;
            if Self::has_ok_cookie(&st) {
                if let Some(t) = st.cookie_obtained {
                    if t.elapsed() < Duration::from_secs(self.cfg.cookie_ttl_secs) {
                        return Ok(());
                    }
                } else {
                    return Ok(());
                }
            }
        }
        // 慢路径：加锁求解
        let mut st = self.state.lock().await;
        // 再检查一次（可能在排队期间已被他人刷新）
        if Self::has_ok_cookie(&st) {
            if let Some(t) = st.cookie_obtained {
                if t.elapsed() < Duration::from_secs(self.cfg.cookie_ttl_secs) {
                    return Ok(());
                }
            }
        }
        if st.solving {
            // 已有其他请求在求解，等待其完成
            drop(st);
            for _ in 0..(self.cfg.solver_timeout_secs + 10) {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let s = self.state.lock().await;
                if Self::has_ok_cookie(&s) {
                    return Ok(());
                }
                if !s.solving {
                    break;
                }
            }
            return Err(AppError::SolverFailed("等待并发求解超时".into()));
        }
        st.solving = true;
        drop(st);

        let result = self.do_auth().await;

        let mut st = self.state.lock().await;
        st.solving = false;
        if result.is_ok() {
            st.cookie_obtained = Some(Instant::now());
        }
        result
    }

    async fn do_auth(&self) -> AppResult<()> {
        tracing::info!("开始 Turnstile 认证流程");
        let token = self.solver.solve().await?;
        tracing::info!("Turnstile 求解成功（token {} 字符）", token.len());

        let url = self.ajax_url();
        let form = [("action", "deepseek_ts_verify"), ("token", token.as_str())];
        let headers = {
            let st = self.state.lock().await;
            self.base_headers(&st)
        };
        let r = self
            .http
            .post(&url)
            .headers(headers)
            .form(&form)
            .send()
            .await
            .map_err(|e| AppError::Network(format!("兑换安全 cookie 失败: {e}")))?;
        let status = r.status();
        let hdrs = r.headers().clone();
        let body = r.text().await.unwrap_or_default();
        {
            let mut st = self.state.lock().await;
            Self::absorb_set_cookie(&mut st, &hdrs);
            // 兑换后 nonce 可能需要刷新
            st.nonce = None;
        }
        #[derive(Deserialize)]
        struct VerifyResp {
            #[serde(default)]
            ok: bool,
        }
        let vr: VerifyResp = serde_json::from_str(&body).unwrap_or(VerifyResp { ok: false });
        if !vr.ok {
            return Err(AppError::SolverFailed(format!(
                "安全校验兑换失败(HTTP {status}): {body}"
            )));
        }
        tracing::info!("安全 cookie 兑换成功");
        Ok(())
    }

    /// 获取（并缓存）nonce。
    pub async fn ensure_nonce(&self) -> AppResult<String> {
        {
            let st = self.state.lock().await;
            if let Some(n) = &st.nonce {
                return Ok(n.clone());
            }
        }
        self.refresh_nonce().await
    }

    pub async fn refresh_nonce(&self) -> AppResult<String> {
        let bot_id = { self.state.lock().await.bot_id.clone() };
        let url = self.ajax_url();
        let form = vec![
            (
                "action".to_string(),
                "aipkit_get_frontend_chat_nonce".to_string(),
            ),
            ("bot_id".to_string(), bot_id),
        ];
        let headers = {
            let st = self.state.lock().await;
            self.base_headers(&st)
        };
        let r = self
            .http
            .post(&url)
            .headers(headers)
            .form(&form)
            .send()
            .await
            .map_err(|e| AppError::Network(format!("获取 nonce 失败: {e}")))?;
        let hdrs = r.headers().clone();
        let body = r.text().await.unwrap_or_default();
        {
            let mut st = self.state.lock().await;
            Self::absorb_set_cookie(&mut st, &hdrs);
        }
        #[derive(Deserialize)]
        struct Resp {
            success: bool,
            #[serde(default)]
            data: Option<NonceData>,
        }
        #[derive(Deserialize)]
        struct NonceData {
            nonce: String,
        }
        let pr: Resp = serde_json::from_str(&body)
            .map_err(|e| AppError::Upstream(format!("解析 nonce 响应失败: {e}, body={body}")))?;
        if !pr.success {
            return Err(AppError::Upstream(format!("nonce 获取失败: {body}")));
        }
        let nonce = pr.data.map(|d| d.nonce).unwrap_or_default();
        if nonce.is_empty() {
            return Err(AppError::Upstream("nonce 为空".into()));
        }
        let mut st = self.state.lock().await;
        st.nonce = Some(nonce.clone());
        Ok(nonce)
    }

    /// 缓存消息换 cache_key。
    pub async fn cache_message(&self, message: &str) -> AppResult<String> {
        let nonce = self.ensure_nonce().await?;
        let bot_id = { self.state.lock().await.bot_id.clone() };
        let url = self.ajax_url();
        let form = vec![
            ("action".to_string(), "aipkit_cache_sse_message".to_string()),
            ("message".to_string(), message.to_string()),
            ("_ajax_nonce".to_string(), nonce),
            ("bot_id".to_string(), bot_id),
        ];
        let headers = {
            let st = self.state.lock().await;
            self.base_headers(&st)
        };
        let r = self
            .http
            .post(&url)
            .headers(headers)
            .form(&form)
            .send()
            .await
            .map_err(|e| AppError::Network(format!("缓存消息失败: {e}")))?;
        let status = r.status();
        let hdrs = r.headers().clone();
        let body = r.text().await.unwrap_or_default();
        {
            let mut st = self.state.lock().await;
            Self::absorb_set_cookie(&mut st, &hdrs);
        }
        if status == reqwest::StatusCode::FORBIDDEN {
            return Err(AppError::TsRequired);
        }
        #[derive(Deserialize)]
        struct Resp {
            success: bool,
            #[serde(default)]
            data: Option<CacheData>,
        }
        #[derive(Deserialize)]
        struct CacheData {
            cache_key: String,
        }
        let pr: Resp = serde_json::from_str(&body)
            .map_err(|e| AppError::Upstream(format!("解析 cache 响应失败: {e}, body={body}")))?;
        if !pr.success {
            // nonce 可能失效
            if body.contains("nonce") || body.contains("security") {
                let _ = self.refresh_nonce().await;
            }
            return Err(AppError::Upstream(format!("缓存失败: {body}")));
        }
        let key = pr.data.map(|d| d.cache_key).unwrap_or_default();
        if key.is_empty() {
            return Err(AppError::Upstream("cache_key 为空".into()));
        }
        Ok(key)
    }

    /// 建立 SSE 流。返回事件流。
    pub async fn stream_chat(
        &self,
        cache_key: &str,
        session_id: &str,
        conversation_uuid: &str,
    ) -> AppResult<impl futures::Stream<Item = AppResult<SseEvent>>> {
        let nonce = self.ensure_nonce().await?;
        let bot_id = { self.state.lock().await.bot_id.clone() };
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let url = format!(
            "{}?action=aipkit_frontend_chat_stream&cache_key={}&bot_id={}&session_id={}&conversation_uuid={}&_ajax_nonce={}&_ts={}",
            self.ajax_url(),
            urlencode(cache_key),
            urlencode(&bot_id),
            urlencode(session_id),
            urlencode(conversation_uuid),
            urlencode(&nonce),
            ts,
        );
        let headers = {
            let st = self.state.lock().await;
            let mut h = self.base_headers(&st);
            h.insert(
                reqwest::header::ACCEPT,
                reqwest::header::HeaderValue::from_static("text/event-stream"),
            );
            h
        };
        let r = self
            .http
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| AppError::Network(format!("建立 SSE 失败: {e}")))?;
        if !r.status().is_success() {
            return Err(AppError::Upstream(format!("SSE HTTP {}", r.status())));
        }
        let bytes_stream = r.bytes_stream();
        Ok(parse_sse_stream(bytes_stream))
    }
}

/// 页面 data-config（部分字段）。
#[derive(Debug, Clone, Deserialize)]
pub struct PageConfig {
    #[serde(rename = "botId", default)]
    pub bot_id: Option<serde_json::Value>,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(rename = "postId", default)]
    pub post_id: Option<serde_json::Value>,
}

/// 从 HTML 中解析 `data-config='...'`。
pub fn parse_page_config(html: &str) -> AppResult<PageConfig> {
    let marker = "data-config='";
    let start = html
        .find(marker)
        .ok_or_else(|| AppError::Upstream("页面未找到 data-config".into()))?;
    let rest = &html[start + marker.len()..];
    let end = rest
        .find('\'')
        .ok_or_else(|| AppError::Upstream("data-config 未闭合".into()))?;
    let raw = decode_entities(&rest[..end]);
    let cfg: PageConfig = serde_json::from_str(&raw)
        .map_err(|e| AppError::Upstream(format!("解析 data-config 失败: {e}")))?;
    Ok(cfg)
}

/// 一个 SSE 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// 把字节流解析为 SSE 事件流（按空行分隔）。
///
/// 规范：SSE 以空行分隔事件；无 `event:` 行的块 = 默认 `message` 事件。
/// 首个 `:` 开头的行是注释（心跳/填充），忽略。
pub fn parse_sse_stream<S>(stream: S) -> impl futures::Stream<Item = AppResult<SseEvent>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    use futures::stream;
    let state = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    stream
        .map(move |chunk| {
            let mut buf = state.lock().unwrap();
            match chunk {
                Ok(b) => {
                    buf.push_str(&String::from_utf8_lossy(&b));
                    // 提取完整事件块
                    let mut events = Vec::new();
                    while let Some(sep) = buf.find("\n\n") {
                        let block = buf[..sep].to_string();
                        *buf = buf[sep + 2..].to_string();
                        if let Some(ev) = parse_block(&block) {
                            events.push(ev);
                        }
                    }
                    events
                }
                Err(e) => {
                    // 用空事件 + 无法表达错误，这里通过特殊事件承载
                    vec![SseEvent {
                        event: "__stream_error__".into(),
                        data: e.to_string(),
                    }]
                }
            }
        })
        .flat_map(|v| stream::iter(v.into_iter().map(Ok)))
}

fn parse_block(block: &str) -> Option<SseEvent> {
    let mut event = "message".to_string();
    let mut data = String::new();
    let mut has_data = false;
    for line in block.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with(':') {
            continue; // 注释/心跳
        }
        if let Some(v) = line.strip_prefix("event:") {
            event = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("data:") {
            has_data = true;
            data.push_str(v.trim());
        }
    }
    if !has_data {
        return None;
    }
    Some(SseEvent { event, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_block_default_event() {
        let ev = parse_block("data: {\"delta\":\"hi\"}").unwrap();
        assert_eq!(ev.event, "message");
        assert_eq!(ev.data, "{\"delta\":\"hi\"}");
    }

    #[test]
    fn parse_block_named_event() {
        let ev = parse_block("event: done\ndata: {\"finished\":true}").unwrap();
        assert_eq!(ev.event, "done");
        assert_eq!(ev.data, "{\"finished\":true}");
    }

    #[test]
    fn parse_block_ignores_comment() {
        assert!(parse_block(": keep-alive").is_none());
    }

    #[test]
    fn parse_block_handles_crlf() {
        let ev = parse_block("event: message_start\r\ndata: {\"message_id\":\"x\"}").unwrap();
        assert_eq!(ev.event, "message_start");
        assert_eq!(ev.data, "{\"message_id\":\"x\"}");
    }

    #[test]
    fn decode_entities_amp() {
        assert_eq!(decode_entities("&#038;"), "&");
    }
}
