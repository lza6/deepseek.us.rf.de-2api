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
    /// 普通请求客户端（AJAX：ts_verify/nonce/cache_message），带总超时
    http: reqwest::Client,
    /// M2：SSE 流式客户端——无总超时，仅连接+空闲读超时
    http_stream: reqwest::Client,
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
    /// 打开至此刻（到达后半开）
    open_until: Option<Instant>,
    /// 是否处于半开态（冷却已过、放行单个探测）
    half_open: bool,
    /// M9：半开态是否已有探测请求在途（保证半开只放行单个探测）
    probe_in_flight: bool,
}

impl UpstreamClient {
    pub fn new(cfg: Config) -> AppResult<Self> {
        let connect = Duration::from_secs(cfg.connect_timeout_secs);
        // 普通 AJAX 客户端：短请求，适用总超时。
        let mut builder = reqwest::Client::builder()
            .cookie_store(false) // 手动管理 cookie
            .connect_timeout(connect)
            .timeout(Duration::from_secs(cfg.http_timeout_secs));
        // M2：SSE 流式客户端——**无总超时**（长回答不应在 120s 处被截断），
        // 仅用连接超时 + 空闲读超时（两次读之间超过 http_timeout_secs 视为上游卡死）。
        let mut sbuild = reqwest::Client::builder()
            .cookie_store(false)
            .connect_timeout(connect)
            .read_timeout(Duration::from_secs(cfg.http_timeout_secs));
        if let Some(p) = &cfg.proxy {
            let proxy = reqwest::Proxy::all(p)
                .map_err(|e| AppError::Internal(format!("代理配置无效: {e}")))?;
            builder = builder.proxy(proxy.clone());
            sbuild = sbuild.proxy(proxy);
        }
        let http = builder
            .build()
            .map_err(|e| AppError::Internal(format!("HTTP 客户端构建失败: {e}")))?;
        let http_stream = sbuild
            .build()
            .map_err(|e| AppError::Internal(format!("流式 HTTP 客户端构建失败: {e}")))?;
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
            http_stream,
            solver,
        })
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

    /// 6.4：熔断器状态快照（供控制台可观测性）。不阻塞：取不到锁返回 "unknown"。
    pub fn breaker_state(&self) -> &'static str {
        match self.breaker.try_lock() {
            Ok(b) => {
                if b.open_until.map(|t| Instant::now() < t).unwrap_or(false) {
                    "open"
                } else if b.half_open {
                    "half_open"
                } else {
                    "closed"
                }
            }
            Err(_) => "unknown",
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

    /// 抓取首页 HTML（余额解析的公共入口）。
    async fn fetch_home_html(&self) -> AppResult<String> {
        let url = format!("{}/", self.cfg.upstream_base_url.trim_end_matches('/'));
        let r = self
            .http
            .get(&url)
            .headers(self.base_headers(&AuthState::default()))
            .send()
            .await
            .map_err(|e| AppError::Network(format!("抓取首页失败: {e}")))?;
        r.text()
            .await
            .map_err(|e| AppError::Network(format!("读取首页失败: {e}")))
    }

    /// 查询 dsgt 余额（`/wp-json/dsgt/v1/balance`）。
    ///
    /// 流程：抓首页 → 解析 `dsgtConfig`（restUrl + nonce）→ 带 `X-WP-Nonce` GET `balance?bot_id=`。
    /// 返回上游原始 JSON（`{balance, free:{remaining}}`）。
    ///
    /// **注意**：余额绑定**浏览器身份**（dsts cookie），返回的是**网关身份**的额度，
    /// 非下游用户余额。用作配额提示是有效的，但不应宣称为"用户余额"。
    pub async fn fetch_balance(&self) -> AppResult<serde_json::Value> {
        let st = self.state.lock().await;
        let bot_id = st.bot_id.clone();
        drop(st);

        let html = self.fetch_home_html().await?;
        let dsgt = parse_dsgt_config(&html)?;
        let base = dsgt.rest_url.trim_end_matches('/');
        let url = format!("{base}/balance?bot_id={}", urlencode(&bot_id));

        let mut headers = {
            let st = self.state.lock().await;
            self.base_headers(&st)
        };
        if let Some(n) = &dsgt.nonce {
            if let Ok(v) = reqwest::header::HeaderValue::from_str(n) {
                headers.insert("X-WP-Nonce", v);
            }
        }
        let r = self
            .http
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| AppError::Network(format!("查询余额失败: {e}")))?;
        let status = r.status();
        let body = r.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(AppError::Upstream(format!(
                "余额查询 HTTP {status}: {body}"
            )));
        }
        serde_json::from_str(&body)
            .map_err(|e| AppError::Upstream(format!("解析余额响应失败: {e}, body={body}")))
    }

    /// 通用 AIPKit admin-ajax 调用（带 nonce，自动吸收 set-cookie）。
    ///
    /// 用于会话管理（list/delete）等非流式端点。
    async fn ajax_call(
        &self,
        action: &str,
        extra: &[(&str, String)],
    ) -> AppResult<serde_json::Value> {
        let nonce = self.ensure_nonce().await?;
        let bot_id = { self.state.lock().await.bot_id.clone() };
        let url = self.ajax_url();
        let mut form: Vec<(String, String)> = vec![
            ("action".to_string(), action.to_string()),
            ("_ajax_nonce".to_string(), nonce),
            ("bot_id".to_string(), bot_id),
        ];
        for (k, v) in extra {
            form.push(((*k).to_string(), v.clone()));
        }
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
            .map_err(|e| AppError::Network(format!("{action} 请求失败: {e}")))?;
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
        if !status.is_success() {
            return Err(AppError::Upstream(format!(
                "{action} HTTP {status}: {body}"
            )));
        }
        serde_json::from_str(&body)
            .map_err(|e| AppError::Upstream(format!("解析 {action} 响应失败: {e}, body={body}")))
    }

    /// 列出会话（`aipkit_get_conversations_list`）。
    ///
    /// 兼容上游响应形状：`data.conversations` 或 `data.items`。
    /// 返回归一化后的列表 `[{id, title}]`。
    pub async fn list_conversations(&self, session_id: &str) -> AppResult<Vec<serde_json::Value>> {
        let v = self
            .ajax_call(
                "aipkit_get_conversations_list",
                &[("session_id", session_id.to_string())],
            )
            .await?;
        let data = v.get("data").cloned().unwrap_or(v);
        let arr = data
            .get("conversations")
            .or_else(|| data.get("items"))
            .and_then(|c| c.as_array())
            .cloned()
            .unwrap_or_default();
        // 归一化：conversation_uuid || uuid || id
        Ok(arr
            .into_iter()
            .map(|c| {
                let id = c
                    .get("conversation_uuid")
                    .or_else(|| c.get("uuid"))
                    .or_else(|| c.get("id"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let title = c
                    .get("title")
                    .or_else(|| c.get("name"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                serde_json::json!({"id": id, "title": title})
            })
            .collect())
    }

    /// 删除单条会话（`aipkit_delete_single_conversation`）。
    pub async fn delete_conversation(
        &self,
        session_id: &str,
        conversation_uuid: &str,
    ) -> AppResult<()> {
        let v = self
            .ajax_call(
                "aipkit_delete_single_conversation",
                &[
                    ("session_id", session_id.to_string()),
                    ("conversation_uuid", conversation_uuid.to_string()),
                ],
            )
            .await?;
        if v.get("success").and_then(|s| s.as_bool()).unwrap_or(false) {
            Ok(())
        } else {
            Err(AppError::Upstream(format!("删除会话失败: {v}")))
        }
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

    /// cookie 剩余 TTL 比例低于该阈值时触发后台预取（M8）。
    const PREFETCH_THRESHOLD: f64 = 0.2;

    /// M8：是否需要预取（cookie 有效但剩余 TTL < 20%）。
    async fn needs_prefetch(&self) -> bool {
        let st = self.state.lock().await;
        if !Self::has_ok_cookie(&st) {
            return false; // 未认证走常规路径
        }
        match st.cookie_obtained {
            Some(t) => {
                let ttl = self.cfg.cookie_ttl_secs as f64;
                let elapsed = t.elapsed().as_secs_f64();
                let remaining = ttl - elapsed;
                remaining > 0.0 && remaining < ttl * Self::PREFETCH_THRESHOLD
            }
            None => false,
        }
    }

    /// M8：启动后台预取任务——提前在 TTL 剩余 20% 时主动续期，
    /// 消除「TTL 到期后首个请求内联阻塞 45-65s」的头阻塞问题。
    ///
    /// 关键：预取时**不清空**旧 cookie（仍有效），仅在 auth_lock 下重新求解并覆盖，
    /// 因此并发请求在续期期间仍走快速路径、不被阻塞。
    pub fn spawn_prefetch(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // 每 30s 检查一次；仅在临近过期时续期，避免无谓求解。
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // 跳过首个立即 tick（启动时不必预取）
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if self.needs_prefetch().await {
                    tracing::info!("cookie 临近过期，后台预取续期（M8）");
                    let _guard = self.auth_lock.lock().await;
                    // 持锁后复查：期间可能已被常规请求刷新
                    if self.needs_prefetch().await {
                        if let Err(e) = self.do_auth().await {
                            tracing::warn!("后台预取失败: {e}（下次请求将重试）");
                        } else {
                            tracing::info!("后台预取成功，cookie 已续期");
                        }
                    }
                }
            }
        })
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
    /// 状态机：闭合 → （连续失败达阈值）打开 → （冷却到点）半开 → 成功则闭合 / 失败则重开。
    ///
    /// M9：半开态**只放行一个探测请求**——首个到者置 `probe_in_flight`，
    /// 其余请求在此期间被拒（返回 false），避免冷却到点后 N 个并发同时打爆 cf_solver。
    async fn breaker_allow(&self) -> bool {
        let mut b = self.breaker.lock().await;
        if let Some(t) = b.open_until {
            if Instant::now() < t {
                return false; // 仍在冷却
            }
            // 冷却到点 → 进入半开
            b.open_until = None;
            b.half_open = true;
            b.probe_in_flight = false;
        }
        if b.half_open {
            if b.probe_in_flight {
                return false; // 已有探测在途，拒绝其他并发
            }
            b.probe_in_flight = true; // 放行本次探测
        }
        true
    }

    /// M9：释放半开探测标记（请求结束时调用，无论成败）。
    async fn breaker_release_probe(&self) {
        let mut b = self.breaker.lock().await;
        b.probe_in_flight = false;
    }

    async fn breaker_on_success(&self) {
        let mut b = self.breaker.lock().await;
        b.consecutive_failures = 0;
        b.open_until = None;
        b.half_open = false;
        b.probe_in_flight = false;
    }

    async fn breaker_on_failure(&self) {
        let mut b = self.breaker.lock().await;
        // M9：半开探测失败 → 立即重新打开（不经过阈值累积），保护滞后问题修复。
        if b.half_open {
            b.half_open = false;
            b.open_until =
                Some(Instant::now() + Duration::from_secs(self.cfg.breaker_cooldown_secs));
            tracing::warn!(
                "半开探测失败，熔断重新打开 {} 秒",
                self.cfg.breaker_cooldown_secs
            );
            return;
        }
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
        // M9：确保半开探测标记在本次认证结束时释放（无论成败）
        let result = self.do_auth_inner().await;
        self.breaker_release_probe().await;
        result
    }

    async fn do_auth_inner(&self) -> AppResult<()> {
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
    ///
    /// M5：若首次失败原因疑似 nonce/security 失效，则刷新 nonce 后**重试一次**
    /// （`cache_sse_message` 对同一 message 幂等，重试安全）。
    pub async fn cache_message(&self, message: &str) -> AppResult<String> {
        let first = self.cache_message_once(message).await;
        match &first {
            Err(AppError::Upstream(b)) if b.contains("nonce") || b.contains("security") => {
                tracing::warn!("cache_message 疑似 nonce 失效，刷新 nonce 后重试一次");
                self.refresh_nonce().await?;
                self.cache_message_once(message).await
            }
            _ => first,
        }
    }

    /// 单次 cache_message（不含 nonce 失效重试）。
    async fn cache_message_once(&self, message: &str) -> AppResult<String> {
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
        // M2：SSE 走独立流式客户端（无总超时，长回答不被截断）
        let r = self
            .http_stream
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| AppError::Network(format!("建立 SSE 失败: {e}")))?;
        // M5：403 视为安全校验失效（可触发上层重认证重试），而非普通上游错误
        if r.status() == reqwest::StatusCode::FORBIDDEN {
            return Err(AppError::TsRequired);
        }
        if !r.status().is_success() {
            return Err(AppError::Upstream(format!("SSE HTTP {}", r.status())));
        }
        let bytes_stream = r.bytes_stream();
        Ok(parse_sse_stream(bytes_stream))
    }
}

/// dsgt 前端配置（余额/订单 REST 所需）。
#[derive(Debug, Clone, Deserialize)]
pub struct DsgtConfig {
    #[serde(rename = "restUrl")]
    pub rest_url: String,
    #[serde(default)]
    pub nonce: Option<String>,
}

/// 从 HTML 中解析 `dsgtConfig = { ... }`（用于余额查询）。
///
/// 页面形如：`dsgtConfig = {"restUrl":"https:\/\/...","nonce":"0c69ef9c1b",...}`。
/// 用大括号配平提取 JSON（容忍字符串内的转义）。
pub fn parse_dsgt_config(html: &str) -> AppResult<DsgtConfig> {
    let key = html
        .find("dsgtConfig")
        .ok_or_else(|| AppError::Upstream("页面未找到 dsgtConfig".into()))?;
    let rest = &html[key..];
    let brace = rest
        .find('{')
        .ok_or_else(|| AppError::Upstream("dsgtConfig 无 JSON 对象".into()))?;
    let body = &rest[brace..];
    // 大括号配平（考虑字符串与转义）
    let mut depth = 0i32;
    let mut in_str = false;
    let mut escaped = false;
    let mut end = None;
    for (i, ch) in body.char_indices() {
        if in_str {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_str = false;
            }
            continue;
        }
        match ch {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.ok_or_else(|| AppError::Upstream("dsgtConfig JSON 未闭合".into()))?;
    let cfg: DsgtConfig = serde_json::from_str(&body[..end])
        .map_err(|e| AppError::Upstream(format!("解析 dsgtConfig 失败: {e}")))?;
    Ok(cfg)
}

/// 一个 SSE 事件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

/// SSE 解析缓冲上限（字节）。超过则判定上游异常（无分隔符的长流），
/// 发出错误并停止，避免内存无界增长。
pub const MAX_SSE_BUF_BYTES: usize = 8 * 1024 * 1024;

/// 在字节缓冲中查找最早的 SSE 事件分隔符，返回 (偏移, 分隔符长度)。
///
/// 支持 SSE 规范允许的三种行尾：`\n\n`、`\r\n\r\n`、`\r\r`。
/// 返回最早出现的分隔符（按起始偏移最小），保证不跨事件误并。
fn find_block_sep(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = find_subslice(buf, b"\n\n").map(|i| (i, 2));
    let crlf = find_subslice(buf, b"\r\n\r\n").map(|i| (i, 4));
    let cr = find_subslice(buf, b"\r\r").map(|i| (i, 2));
    [lf, crlf, cr].into_iter().flatten().min_by_key(|(i, _)| *i)
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// 把字节流解析为 SSE 事件流（按空行分隔）。
///
/// 规范：SSE 以空行分隔事件；无 `event:` 行的块 = 默认 `message` 事件。
/// 首个 `:` 开头的行是注释（心跳/填充），忽略。
/// 流结束时 flush 残留块——即使上游末尾没有空行也不会丢最后一个事件（P2-4）。
///
/// **H1 修复**：缓冲**原始字节**（`Vec<u8>`），只在完整事件边界解码。
/// 避免 `from_utf8_lossy` 在 chunk 边界切断多字节码点时把 CJK/emoji 损坏为 U+FFFD。
/// 不完整的尾字节保留在缓冲区，等下一个 chunk 补齐（或流结束按 lossy 兜底）。
pub fn parse_sse_stream<S>(stream: S) -> impl futures::Stream<Item = AppResult<SseEvent>>
where
    S: futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    struct St<S> {
        s: std::pin::Pin<Box<S>>,
        buf: Vec<u8>,
        done: bool,
        /// 待发出的流错误（缓冲排空后再发，保证顺序）
        pending_err: Option<String>,
    }
    futures::stream::unfold(
        St {
            s: Box::pin(stream),
            buf: Vec::new(),
            done: false,
            pending_err: None,
        },
        |mut st| async move {
            loop {
                // 优先消费缓冲区中已完整的事件块（无论来自本次还是上次 chunk）。
                // 关键：必须在 await 前先 drain，否则单次 chunk 内的多个事件会被丢弃。
                if let Some((sep, seplen)) = find_block_sep(&st.buf) {
                    let block = st.buf[..sep].to_vec();
                    st.buf.drain(..sep + seplen);
                    // 完整块在分隔符处已保证是完整 UTF-8（分隔符本身是 ASCII）
                    if let Some(ev) = parse_block_bytes(&block) {
                        return Some((Ok(ev), st));
                    }
                    continue;
                }
                if st.done {
                    // 流结束：flush 残留块（无尾随空行时不丢事件）
                    if !st.buf.iter().all(|b| b.is_ascii_whitespace()) {
                        // 末尾残留可能是被截断的多字节字符：用 lossy 兜底解码（不 panic）
                        let block = std::mem::take(&mut st.buf);
                        if let Some(ev) = parse_block_bytes(&block) {
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
                        st.buf.extend_from_slice(&b);
                        // 上限保护：无分隔符的超大流判定异常
                        if st.buf.len() > MAX_SSE_BUF_BYTES {
                            tracing::warn!(
                                "SSE 缓冲超过 {} 字节仍无分隔符，判定上游异常",
                                MAX_SSE_BUF_BYTES
                            );
                            st.done = true;
                            st.pending_err = Some("上游流异常：单事件超过大小上限".to_string());
                            st.buf.clear();
                        }
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

/// 从完整事件块的**原始字节**解析事件。
///
/// 块内已是完整 UTF-8（由分隔符保证），但末尾可能残留被截断的多字节字符，
/// 故用 `from_utf8` 成功优先、失败时 `from_utf8_lossy` 兜底（不丢事件、不 panic）。
fn parse_block_bytes(block: &[u8]) -> Option<SseEvent> {
    let text: std::borrow::Cow<'_, str> = match std::str::from_utf8(block) {
        Ok(s) => std::borrow::Cow::Borrowed(s),
        Err(_) => String::from_utf8_lossy(block),
    };
    parse_block(&text)
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

    // ── dsgtConfig 解析（余额端点，批次 C） ──────────────────

    #[test]
    fn parse_dsgt_config_basic() {
        let html = r#"<script>var dsgtConfig = {"restUrl":"https:\/\/deepseek.es\/wp-json\/dsgt\/v1\/","nonce":"0c69ef9c1b","mayBuy":true};</script>"#;
        let c = parse_dsgt_config(html).unwrap();
        assert!(c.rest_url.contains("dsgt/v1"), "{}", c.rest_url);
        assert_eq!(c.nonce.as_deref(), Some("0c69ef9c1b"));
    }

    #[test]
    fn parse_dsgt_config_nested_and_escapes() {
        // 含嵌套对象与字符串内花括号/转义，配平必须正确
        let html = r#"<x>dsgtConfig = {"restUrl":"https://x/y/","i18n":{"a":"{not a brace}","b":"quote\"here"}};</x>"#;
        let c = parse_dsgt_config(html).unwrap();
        assert_eq!(c.rest_url, "https://x/y/");
    }

    #[test]
    fn parse_dsgt_config_missing_errors() {
        assert!(parse_dsgt_config("<html>no config</html>").is_err());
    }

    #[test]
    fn parse_dsgt_config_unclosed_errors() {
        assert!(parse_dsgt_config("dsgtConfig = {\"restUrl\":\"x\"").is_err());
    }

    // ── H1 回归：多字节 UTF-8 跨 chunk 边界不得损坏 ──────────────

    #[tokio::test]
    async fn sse_multibyte_split_across_chunks() {
        use futures::StreamExt;
        // "你好" = E4 BD A0 E5 A5 BD。把第二个字符从中间切开分两个 chunk 发出。
        // 修复前：from_utf8_lossy 会把两半各替换为 U+FFFD → "好" 损坏。
        let full = "data: {\"delta\":\"你好\"}\n\n".as_bytes().to_vec();
        // 在 "你好" 内部（第 4 字节处）切分
        let cut = full.windows(2).position(|w| w == [0xE5, 0xA5]).unwrap();
        let (a, b) = full.split_at(cut);
        let chunks: Vec<Result<bytes::Bytes, reqwest::Error>> = vec![
            Ok(bytes::Bytes::copy_from_slice(a)),
            Ok(bytes::Bytes::copy_from_slice(b)),
        ];
        let s = parse_sse_stream(futures::stream::iter(chunks));
        let evs: Vec<_> = s.collect().await;
        assert_eq!(evs.len(), 1, "应产出 1 个事件: {evs:?}");
        let ev = evs[0].as_ref().unwrap();
        assert_eq!(
            ev.data, "{\"delta\":\"你好\"}",
            "多字节字符被损坏: {}",
            ev.data
        );
        assert!(
            !ev.data.contains('\u{FFFD}'),
            "出现替换符 U+FFFD: {}",
            ev.data
        );
    }

    #[tokio::test]
    async fn sse_multibyte_split_at_every_boundary() {
        use futures::StreamExt;
        // 逐字节切分（最极端），每个 chunk 仅 1 字节，验证缓冲正确重组。
        let full = "data: {\"delta\":\"漢字テスト😀\"}\n\n".as_bytes().to_vec();
        let chunks: Vec<Result<bytes::Bytes, reqwest::Error>> = full
            .chunks(1)
            .map(|c| Ok(bytes::Bytes::copy_from_slice(c)))
            .collect();
        let s = parse_sse_stream(futures::stream::iter(chunks));
        let evs: Vec<_> = s.collect().await;
        assert_eq!(evs.len(), 1);
        let ev = evs[0].as_ref().unwrap();
        assert_eq!(
            ev.data, "{\"delta\":\"漢字テスト😀\"}",
            "逐字节切分损坏: {}",
            ev.data
        );
    }

    #[tokio::test]
    async fn sse_partial_utf8_left_in_buffer_no_replacement() {
        use futures::StreamExt;
        // 第一个 chunk 以半个 3 字节字符结尾：该半字符必须留在缓冲区，
        // 不得立即产生替换符；下一个 chunk 补齐后应得到完整字符。
        let (a, b) = "data: {\"delta\":\"中\"}\n\n".as_bytes().split_at(15); // 在 "中" 中间
        let chunks: Vec<Result<bytes::Bytes, reqwest::Error>> = vec![
            Ok(bytes::Bytes::copy_from_slice(a)),
            Ok(bytes::Bytes::copy_from_slice(b)),
        ];
        let s = parse_sse_stream(futures::stream::iter(chunks));
        let evs: Vec<_> = s.collect().await;
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].as_ref().unwrap().data, "{\"delta\":\"中\"}");
    }

    #[tokio::test]
    async fn sse_crlf_block_separator() {
        use futures::StreamExt;
        // M1：上游若用 \r\n\r\n 分隔，必须能正确分帧（否则整段塌缩为一个块）。
        let raw = b"event: message_start\r\ndata: {\"message_id\":\"m\"}\r\n\r\n\
                    data: {\"delta\":\"a\"}\r\n\r\n\
                    event: done\r\ndata: {\"finished\":true}\r\n\r\n";
        let chunks: Vec<Result<bytes::Bytes, reqwest::Error>> =
            vec![Ok(bytes::Bytes::from_static(raw))];
        let s = parse_sse_stream(futures::stream::iter(chunks));
        let evs: Vec<_> = s.collect().await;
        let names: Vec<_> = evs
            .iter()
            .map(|e| e.as_ref().unwrap().event.clone())
            .collect();
        assert_eq!(
            names,
            vec!["message_start", "message", "done"],
            "CRLF 分帧失败: {names:?}"
        );
        assert_eq!(evs[1].as_ref().unwrap().data, "{\"delta\":\"a\"}");
    }

    #[tokio::test]
    async fn sse_oversized_buffer_emits_error_not_oom() {
        use futures::stream;
        use futures::StreamExt;
        // 无分隔符的超大流：必须触发上限保护（错误帧），而非无界增长。
        let big = bytes::Bytes::from(vec![b'x'; MAX_SSE_BUF_BYTES + 1]);
        let chunks: Vec<Result<bytes::Bytes, reqwest::Error>> =
            vec![Ok(big), Ok(bytes::Bytes::from_static(b""))];
        let s = parse_sse_stream(stream::iter(chunks));
        let evs: Vec<_> = s.collect().await;
        let has_err = evs
            .iter()
            .any(|e| matches!(e, Ok(ev) if ev.event == "__stream_error__"));
        assert!(has_err, "超限未触发保护: {evs:?}");
    }

    // ── M9 回归：熔断状态机（含半开单探测） ──────────────────────

    fn test_client(threshold: u32, cooldown: u64) -> UpstreamClient {
        let cfg = Config {
            cf_solver_url: "http://127.0.0.1:1".into(), // 不会实际调用
            breaker_fail_threshold: threshold,
            breaker_cooldown_secs: cooldown,
            ..Default::default()
        };
        UpstreamClient::new(cfg).unwrap()
    }

    #[tokio::test]
    async fn breaker_opens_after_threshold() {
        let c = test_client(3, 60);
        assert!(c.breaker_allow().await, "闭合态应放行");
        c.breaker_on_failure().await;
        c.breaker_on_failure().await;
        assert!(c.breaker_allow().await, "未达阈值应仍放行");
        c.breaker_on_failure().await; // 第 3 次 → 达到阈值
        assert!(!c.breaker_allow().await, "达阈值后应熔断（拒绝）");
    }

    #[tokio::test]
    async fn breaker_half_open_allows_single_probe_m9() {
        // cooldown=0 → 立即进入半开
        let c = test_client(1, 0);
        c.breaker_on_failure().await; // 打开
                                      // 冷却为 0，首次 allow 触发半开并放行（占用探测位）
        assert!(c.breaker_allow().await, "半开应放行首个探测");
        // 第二个并发探测在半开且已被占用时必须被拒
        assert!(!c.breaker_allow().await, "半开应拒绝并发探测（M9）");
        // 释放后再次放行
        c.breaker_release_probe().await;
        assert!(c.breaker_allow().await, "释放后应可再次探测");
    }

    #[tokio::test]
    async fn breaker_half_open_failure_reopens_immediately_m9() {
        let c = test_client(1, 60);
        c.breaker_on_failure().await; // 打开（60s 冷却）
                                      // 手动把冷却起点推前，模拟冷却已过 → 下次 allow 进入半开
        {
            let mut b = c.breaker.lock().await;
            b.open_until = Some(Instant::now() - Duration::from_secs(1));
        }
        assert!(c.breaker_allow().await, "冷却过后应半开放行探测");
        // 半开探测失败 → 立即重开（不等阈值累积）
        c.breaker_on_failure().await;
        assert!(!c.breaker_allow().await, "半开失败应立即重新熔断（M9）");
    }

    #[tokio::test]
    async fn breaker_success_closes() {
        let c = test_client(2, 60);
        c.breaker_on_failure().await;
        c.breaker_on_failure().await; // 打开
        assert!(!c.breaker_allow().await);
        c.breaker_on_success().await; // 手动恢复
        assert!(c.breaker_allow().await, "成功后应闭合");
    }

    // ── M2：流式客户端与普通客户端分离 ──────────────────────────

    #[tokio::test]
    async fn m2_stream_client_has_no_total_timeout() {
        // 两个客户端都能成功构造即视为 API 可用（行为差异由 reqwest 保证）
        let c = test_client(5, 30);
        let _ = &c.http;
        let _ = &c.http_stream;
    }

    // ── M5：cache_message 疑似 nonce 失效判定 ──────────────────

    #[test]
    fn m5_nonce_failure_detection() {
        // 判定逻辑：错误串包含 "nonce"/"security" 才触发刷新重试
        let is_nonce_err = |s: &str| s.contains("nonce") || s.contains("security");
        assert!(is_nonce_err("缓存失败: invalid nonce"));
        assert!(is_nonce_err("缓存失败: security check failed"));
        assert!(!is_nonce_err("缓存失败: quota exceeded"));
    }

    // ── M8：预取阈值判定 ────────────────────────────────────────

    #[tokio::test]
    async fn m8_needs_prefetch_true_when_near_expiry() {
        let c = test_client(5, 30);
        // 未认证 → 不预取
        assert!(!c.needs_prefetch().await, "未认证不应预取");
        // 手动注入：已认证 + cookie 获取于 85% TTL 之前（剩余 15% < 20%）
        {
            let mut st = c.state.lock().await;
            st.cookies = vec![("dsts_ok".into(), "1".into())];
            // cookie_ttl_secs 默认 1800；设为 1520s 前获取 → 剩余 280s ≈ 15.5%
            st.cookie_obtained = Some(Instant::now() - Duration::from_secs(1700));
        }
        assert!(c.needs_prefetch().await, "剩余 TTL < 20% 应触发预取");
        // 新鲜 cookie（刚获取）→ 不预取
        {
            let mut st = c.state.lock().await;
            st.cookie_obtained = Some(Instant::now());
        }
        assert!(!c.needs_prefetch().await, "新鲜 cookie 不应预取");
    }
}
