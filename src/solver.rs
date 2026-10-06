//! Cloudflare Turnstile 求解客户端（对接 tools/cf_solver 服务）。
//!
//! cf_solver 契约（源自上层项目 imagefree-2ai/deploy/cf_solver）：
//!   GET /turnstile?url=&sitekey=[&action=]  → 202 {task_id, status:"accepted"}
//!   GET /result?id=<task_id>                → 200 {status:"success", value:<token>}
//!                                           → 200 {status:"process"} 求解中
//!                                           → 404 未知/过期
//!                                           → 429 限流
//!
//! deepseek.es 安全层：Turnstile token → POST admin-ajax action=deepseek_ts_verify → dsts_ok cookie。

use crate::config::Config;
use crate::errors::{AppError, AppResult};
use serde::Deserialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Deserialize)]
struct SolveAccepted {
    task_id: String,
}

#[derive(Debug, Deserialize)]
struct SolveResult {
    status: String,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// 单个 solver 实例的运行时状态（健康度 + 连续失败计数）。
#[derive(Debug, Default)]
struct SolverHealth {
    /// 连续失败次数（成功后归零）
    consecutive_failures: u32,
    /// 最近一次成功求解耗时（秒）
    last_success_secs: Option<f64>,
    /// 累计成功/失败
    total_success: u64,
    total_failure: u64,
}

/// 多 solver 实例的负载均衡器（轮询 + 故障转移 + 健康探测）。
pub struct SolverPool {
    http: reqwest::Client,
    /// 实例地址列表（非空）
    urls: Vec<String>,
    /// 健康状态（与 urls 同序，Arc 以便共享读取）
    health: Arc<std::sync::Mutex<Vec<SolverHealth>>>,
    /// 轮询游标
    cursor: AtomicUsize,
    sitekey: String,
    site_url: String,
    timeout: Duration,
}

impl SolverPool {
    pub fn new(cfg: &Config) -> AppResult<Self> {
        let mut builder = reqwest::Client::builder()
            .timeout(Duration::from_secs(cfg.solver_timeout_secs + 30))
            .danger_accept_invalid_certs(false);
        // 求解器通常在本地，无需代理；若配置了代理则使用（求解器可能需直连 CF）
        if let Some(p) = &cfg.proxy {
            if let Ok(px) = reqwest::Proxy::all(p) {
                builder = builder.proxy(px);
            }
        }
        let http = builder
            .build()
            .map_err(|e| AppError::SolverFailed(format!("构建求解器 HTTP 客户端失败: {e}")))?;

        // 实例列表：solver_urls 优先；为空则回退单实例 cf_solver_url
        let mut urls: Vec<String> = cfg
            .solver_urls
            .iter()
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty())
            .collect();
        if urls.is_empty() {
            let single = cfg.cf_solver_url.trim_end_matches('/').to_string();
            if !single.is_empty() {
                urls.push(single);
            }
        }
        if urls.is_empty() {
            return Err(AppError::SolverFailed("未配置任何 cf_solver 地址".into()));
        }
        let n = urls.len();
        Ok(SolverPool {
            http,
            urls,
            health: Arc::new(std::sync::Mutex::new(
                (0..n).map(|_| SolverHealth::default()).collect(),
            )),
            cursor: AtomicUsize::new(0),
            sitekey: cfg.sitekey.clone(),
            site_url: format!("{}/", cfg.upstream_base_url.trim_end_matches('/')),
            timeout: Duration::from_secs(cfg.solver_timeout_secs),
        })
    }

    /// 实例数量。
    pub fn len(&self) -> usize {
        self.urls.len()
    }

    pub fn is_empty(&self) -> bool {
        self.urls.is_empty()
    }

    /// 按健康度选择下一个实例下标（跳过连续失败 ≥3 的实例，除非全部不健康）。
    fn pick(&self) -> usize {
        let n = self.urls.len();
        let start = self.cursor.fetch_add(1, Ordering::Relaxed) % n;
        let health = self.health.lock().unwrap();
        for i in 0..n {
            let idx = (start + i) % n;
            if health[idx].consecutive_failures < 3 {
                return idx;
            }
        }
        // 全部不健康：仍返回轮询位置（避免永久拒绝服务）
        start
    }

    fn record_success(&self, idx: usize, elapsed: f64) {
        let mut h = self.health.lock().unwrap();
        h[idx].consecutive_failures = 0;
        h[idx].last_success_secs = Some(elapsed);
        h[idx].total_success += 1;
    }

    fn record_failure(&self, idx: usize) {
        let mut h = self.health.lock().unwrap();
        h[idx].consecutive_failures += 1;
        h[idx].total_failure += 1;
    }

    /// 健康快照（供控制台/诊断）。
    pub fn health_snapshot(&self) -> Vec<serde_json::Value> {
        let h = self.health.lock().unwrap();
        self.urls
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let s = &h[i];
                serde_json::json!({
                    "url": u,
                    "healthy": s.consecutive_failures < 3,
                    "consecutive_failures": s.consecutive_failures,
                    "last_success_secs": s.last_success_secs,
                    "total_success": s.total_success,
                    "total_failure": s.total_failure,
                })
            })
            .collect()
    }

    /// 求解 Turnstile（多实例轮询 + 单实例内失败即换下一个）。
    ///
    /// 每个实例尝试一轮；全部失败则返回最后一个错误。
    pub async fn solve(&self) -> AppResult<String> {
        let n = self.urls.len();
        let mut last_err = AppError::SolverFailed("无可用求解器".into());
        let mut tried: Vec<usize> = Vec::with_capacity(n);
        for _ in 0..n {
            let idx = self.pick();
            if tried.contains(&idx) {
                break;
            }
            tried.push(idx);
            match self.solve_one(idx).await {
                Ok(t) => return Ok(t),
                Err(e) => {
                    tracing::warn!("求解器实例 {} 失败: {}", self.urls[idx], e);
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    /// 在指定实例上求解（提交 + 轮询）。
    async fn solve_one(&self, idx: usize) -> AppResult<String> {
        let base = &self.urls[idx];
        let started = Instant::now();
        let url = format!(
            "{}/turnstile?url={}&sitekey={}&action=chat",
            base,
            urlencode(&self.site_url),
            urlencode(&self.sitekey),
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::SolverFailed(format!("提交求解任务失败: {e}")))?;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            self.record_failure(idx);
            return Err(AppError::SolverFailed("求解器限流(429)".into()));
        }
        if !resp.status().is_success() {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            self.record_failure(idx);
            return Err(AppError::SolverFailed(format!("求解器返回 {code}: {body}")));
        }
        let accepted: SolveAccepted = resp
            .json()
            .await
            .map_err(|e| AppError::SolverFailed(format!("解析求解任务响应失败: {e}")))?;

        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            if tokio::time::Instant::now() >= deadline {
                self.record_failure(idx);
                return Err(AppError::SolverFailed("Turnstile 求解超时".into()));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;

            let rurl = format!("{base}/result?id={}", urlencode(&accepted.task_id));
            let r = self.http.get(&rurl).send().await;
            let r = match r {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!("轮询求解结果失败(重试): {e}");
                    continue;
                }
            };
            let status = r.status();
            if status == reqwest::StatusCode::NOT_FOUND {
                self.record_failure(idx);
                return Err(AppError::SolverFailed("求解任务过期/未知".into()));
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                self.record_failure(idx);
                return Err(AppError::SolverFailed("求解器限流(429)".into()));
            }
            let sr: SolveResult = match r.json().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("解析求解结果失败(重试): {e}");
                    continue;
                }
            };
            match sr.status.as_str() {
                "success" => {
                    let token = sr.value.unwrap_or_default();
                    if token.is_empty() {
                        self.record_failure(idx);
                        return Err(AppError::SolverFailed("求解成功但 token 为空".into()));
                    }
                    self.record_success(idx, started.elapsed().as_secs_f64());
                    return Ok(token);
                }
                "process" | "processing" => continue,
                "error" => {
                    self.record_failure(idx);
                    return Err(AppError::SolverFailed(
                        sr.message.unwrap_or_else(|| "求解失败".into()),
                    ));
                }
                other => {
                    self.record_failure(idx);
                    return Err(AppError::SolverFailed(format!("未知求解状态: {other}")));
                }
            }
        }
    }
}

/// 最小 percent-encoding（避免额外依赖）。
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urlencode_basic() {
        assert_eq!(urlencode("abcXYZ123-_.~"), "abcXYZ123-_.~");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("https://x.es/"), "https%3A%2F%2Fx.es%2F");
        assert_eq!(urlencode("0x4AAAA=="), "0x4AAAA%3D%3D");
    }
}
