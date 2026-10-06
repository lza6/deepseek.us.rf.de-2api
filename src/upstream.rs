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
use crate::solver::{urlencode, SolverPool};
use futures::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// 上游客户端（携带 cookie jar + 缓存的 nonce/cookie）。
pub struct UpstreamClient {
    cfg: Config,
    http: reqwest::Client,
    solver: Arc<SolverPool>,
    /// 缓存的安全 cookie 状态
    state: Mutex<AuthState>,
    /// 认证串行化锁：确保同一时刻只有一个求解在途。
    /// 等待者在拿到锁后重新检查状态——若前一次求解失败，则自己接手（修复旧实现"连坐失败"）。
    auth_lock: tokio::sync::Mutex<()>,
    /// 认证熔断状态（保护 cf_solver 单点）
    breaker: Mutex<BreakerState>,
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
}

/// 熔断器状态：连续失败达阈值后打开，冷却后半开。
#[derive(Debug, Default)]
struct BreakerState {
    consecutive_failures: u32,
    open_until: Option<Instant>,
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
        let solver = Arc::new(SolverPool::new(&cfg)?);
        Ok(UpstreamClient {
            state: Mutex::new(AuthState {
                bot_id: cfg.bot_id.clone(),
                ..Default::default()
            }),
            auth_lock: tokio::sync::Mutex::new(()),
            breaker: Mutex::new(BreakerState::default()),
            cfg,
            http,
            solver,
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// 求解器健康快照（供控制台）。
    pub fn solver_health(&self) -> Vec<serde_json::Value> {
        self.solver.health_snapshot()
    }

    /// 求解器实例数量。
    pub fn solver_count(&self) -> usize {
        self.solver.len()
    }

    /// 当前是否持有有效认证（供控制台；不触发求解）。
    pub fn is_authed_cached(&self) -> bool {
        match self.state.try_lock() {
            Ok(st) => {
                Self::has_ok_cookie(&st)
                    && st
                        .cookie_obtained
                        .map(|t| t.elapsed() < Duration::from_secs(self.cfg.cookie_ttl_secs))
                        .unwrap_or(false)
            }
            Err(_) => false,
        }
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
        // WP admin-ajax 惯例头（此前为死代码，现真正插入）
        h.insert(
            HeaderName::from_static("x-requested-with"),
            HeaderValue::from_static("XMLHttpRequest"),
        );
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

    /// 是否持有有效的 dsts_ok cookie。
    async fn is_authed(&self) -> bool {
        let st = self.state.lock().await;
        if !Self::has_ok_cookie(&st) {
            return false;
        }
        match st.cookie_obtained {
            Some(t) => t.elapsed() < Duration::from_secs(self.cfg.cookie_ttl_secs),
            // 无获取时刻视为无效（修复旧实现"cookie 存在即视为有效"的隐患）
            None => false,
        }
    }

    /// 确保已认证（有 dsts_ok cookie 且未过期）。
    ///
    /// 并发语义：快速路径无锁；需要求解时通过 `auth_lock` 串行化。
    /// 等待者在拿到锁后**重新检查**——若前一次求解失败，则自己接手求解，
    /// 而非直接返回错误（修复旧实现用 `solving` 布尔量导致的"连坐失败"）。
    pub async fn ensure_authed(&self) -> AppResult<()> {
        if self.is_authed().await {
            return Ok(());
        }
        let _guard = self.auth_lock.lock().await;
        // 拿到锁后再检查一次：期间可能已被其他请求刷新
        if self.is_authed().await {
            return Ok(());
        }
        self.do_auth().await
    }

    /// 强制失效当前认证状态并重新求解。
    ///
    /// 用于 `ts_required` 自愈：上游可能在 cookie 尚未到 TTL 时就判其失效，
    /// 此时必须显式清空本地缓存（cookie/nonce），否则 `ensure_authed` 的快速路径
    /// 会误判为"仍有效"而拒绝刷新。
    pub async fn force_reauth(&self) -> AppResult<()> {
        let _guard = self.auth_lock.lock().await;
        {
            let mut st = self.state.lock().await;
            st.cookies.clear();
            st.cookie_obtained = None;
            st.nonce = None;
        }
        tracing::warn!("认证状态被强制失效，重新求解 Turnstile");
        self.do_auth().await
    }

    /// 熔断是否放行（打开期间快速失败）。
    ///
    /// 冷却到点后进入「半开」：清空 `open_until`，使后续失败重新从 0 计数，
    /// 避免冷却后一失败就立即再次打开、且失败计数无限累积。
    async fn breaker_allow(&self) -> bool {
        let mut b = self.breaker.lock().await;
        if let Some(t) = b.open_until {
            if Instant::now() < t {
                return false;
            }
            b.open_until = None;
            b.consecutive_failures = 0;
        }
        true
    }

    async fn breaker_on_success(&self) {
        let mut b = self.breaker.lock().await;
        b.consecutive_failures = 0;
        b.open_until = None;
    }

    async fn breaker_on_failure(&self) {
        let mut b = self.breaker.lock().await;
        b.consecutive_failures += 1;
        if b.consecutive_failures >= self.cfg.breaker_fail_threshold {
            b.open_until =
                Some(Instant::now() + Duration::from_secs(self.cfg.breaker_cooldown_secs));
            tracing::warn!(
                "认证连续失败 {} 次，熔断 {} 秒",
                b.consecutive_failures,
                self.cfg.breaker_cooldown_secs
            );
        }
    }

    /// 求解 Turnstile（带指数退避重试）。每次重试前等待 2^i 秒（上限 8s）。
    async fn solve_with_retry(&self) -> AppResult<String> {
        let attempts = self.cfg.solver_retries + 1;
        let mut last_err = AppError::SolverFailed("求解失败".into());
        for i in 0..attempts {
            if i > 0 {
                // 指数退避 2^i 秒，上限 8s。
                // 先夹指数再移位，避免 solver_retries>=64 时 1u64<<i 在 debug 构建下溢出 panic。
                let backoff = Duration::from_secs(1u64 << i.min(3));
                tracing::warn!(
                    "Turnstile 求解重试 {}/{}（等待 {:?}）",
                    i + 1,
                    attempts,
                    backoff
                );
                tokio::time::sleep(backoff).await;
            }
            match self.solver.solve().await {
                Ok(t) => return Ok(t),
                Err(e) => {
                    tracing::warn!("Turnstile 求解尝试 {} 失败: {}", i + 1, e);
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    async fn do_auth(&self) -> AppResult<()> {
        if !self.breaker_allow().await {
            return Err(AppError::SolverFailed(
                "认证熔断中（近期连续失败），请稍后重试".into(),
            ));
        }
        tracing::info!("开始 Turnstile 认证流程");
        let token = match self.solve_with_retry().await {
            Ok(t) => t,
            Err(e) => {
                self.breaker_on_failure().await;
                return Err(e);
            }
        };
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
            self.breaker_on_failure().await;
            return Err(AppError::SolverFailed(format!(
                "安全校验兑换失败(HTTP {status}): {body}"
            )));
        }
        // 记录认证时刻（供 TTL 判定）
        {
            let mut st = self.state.lock().await;
            st.cookie_obtained = Some(Instant::now());
        }
        self.breaker_on_success().await;
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
/// 流结束时 flush 残留块——即使上游末尾没有空行也不会丢最后一个事件（P2-4）。
pub fn parse_sse_stream<S>(stream: S) -> impl futures::Stream<Item = AppResult<SseEvent>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    struct St<S> {
        s: std::pin::Pin<Box<S>>,
        buf: String,
        done: bool,
        /// 待发出的流错误（缓冲排空后再发，保证顺序）
        pending_err: Option<String>,
    }
    futures::stream::unfold(
        St {
            s: Box::pin(stream),
            buf: String::new(),
            done: false,
            pending_err: None,
        },
        |mut st| async move {
            loop {
                // 优先消费缓冲区中已完整的事件块（无论来自本次还是上次 chunk）。
                // 关键：必须在 await 前先 drain，否则单次 chunk 内的多个事件会被丢弃。
                if let Some(sep) = st.buf.find("\n\n") {
                    let block = st.buf[..sep].to_string();
                    st.buf = st.buf[sep + 2..].to_string();
                    if let Some(ev) = parse_block(&block) {
                        return Some((Ok(ev), st));
                    }
                    continue;
                }
                if st.done {
                    // 流结束：flush 残留块（无尾随空行时不丢事件）
                    if !st.buf.trim().is_empty() {
                        let ev = parse_block(&st.buf);
                        st.buf.clear();
                        if let Some(ev) = ev {
                            return Some((Ok(ev), st));
                        }
                    }
                    // 缓冲区已排空，再发流错误
                    if let Some(msg) = st.pending_err.take() {
                        return Some((
                            Ok(SseEvent {
                                event: "__stream_error__".into(),
                                data: msg,
                            }),
                            st,
                        ));
                    }
                    return None;
                }
                match st.s.next().await {
                    Some(Ok(b)) => {
                        st.buf.push_str(&String::from_utf8_lossy(&b));
                    }
                    Some(Err(e)) => {
                        st.done = true;
                        st.pending_err = Some(e.to_string());
                    }
                    None => {
                        st.done = true;
                    }
                }
            }
        },
    )
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

    #[tokio::test]
    async fn sse_flush_trailing_without_blank_line() {
        use futures::StreamExt;
        // 末尾无空行的 done 事件也应被 flush 出来
        let chunks: Vec<Result<bytes::Bytes, reqwest::Error>> = vec![
            Ok(bytes::Bytes::from_static(b"data: {\"delta\":\"a\"}\n\n")),
            Ok(bytes::Bytes::from_static(
                b"event: done\ndata: {\"finished\":true}",
            )),
        ];
        let s = parse_sse_stream(futures::stream::iter(chunks));
        let evs: Vec<_> = s.collect().await;
        assert_eq!(evs.len(), 2, "末尾块未 flush: {evs:?}");
        let last = evs[1].as_ref().unwrap();
        assert_eq!(last.event, "done");
    }

    #[tokio::test]
    async fn sse_single_chunk_multiple_events() {
        use futures::StreamExt;
        // 关键回归：单个 chunk 内含多个事件（含上游超长填充前缀）必须全部产出，
        // 不能只出前两个（曾因 unfold 在 await 前未 drain 缓冲而丢事件）。
        let raw = b":                        \n\n\
            event: message_start\ndata: {\"message_id\":\"m\"}\n\n\
            data: {\"delta\":\"\"}\n\n\
            data: {\"delta\":\"Hi\"}\n\n\
            event: done\ndata: {\"finished\":true}\n\n";
        let chunks: Vec<Result<bytes::Bytes, reqwest::Error>> =
            vec![Ok(bytes::Bytes::from_static(raw))];
        let s = parse_sse_stream(futures::stream::iter(chunks));
        let evs: Vec<_> = s.collect::<Vec<_>>().await;
        let names: Vec<_> = evs
            .iter()
            .map(|e| e.as_ref().unwrap().event.clone())
            .collect();
        assert_eq!(
            names,
            vec!["message_start", "message", "message", "done"],
            "单 chunk 多事件被丢弃: {names:?}"
        );
    }

    #[test]
    fn decode_entities_amp() {
        assert_eq!(decode_entities("&#038;"), "&");
    }
}
