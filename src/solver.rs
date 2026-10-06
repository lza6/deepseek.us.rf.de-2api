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
use std::time::Duration;

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

/// 纯 HTTP 客户端（求解任务提交/轮询）。
pub struct SolverClient {
    http: reqwest::Client,
    base_url: String,
    sitekey: String,
    site_url: String,
    timeout: Duration,
}

impl SolverClient {
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
        Ok(SolverClient {
            http,
            base_url: cfg.cf_solver_url.trim_end_matches('/').to_string(),
            sitekey: cfg.sitekey.clone(),
            site_url: format!("{}/", cfg.upstream_base_url.trim_end_matches('/')),
            timeout: Duration::from_secs(cfg.solver_timeout_secs),
        })
    }

    /// 求解 Turnstile，返回 token。阻塞直到成功/超时。
    pub async fn solve(&self) -> AppResult<String> {
        let url = format!(
            "{}/turnstile?url={}&sitekey={}&action=chat",
            self.base_url,
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
            return Err(AppError::SolverFailed("求解器限流(429)".into()));
        }
        if !resp.status().is_success() {
            let code = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AppError::SolverFailed(format!("求解器返回 {code}: {body}")));
        }
        let accepted: SolveAccepted = resp
            .json()
            .await
            .map_err(|e| AppError::SolverFailed(format!("解析求解任务响应失败: {e}")))?;

        let deadline = tokio::time::Instant::now() + self.timeout;
        loop {
            if tokio::time::Instant::now() >= deadline {
                return Err(AppError::SolverFailed("Turnstile 求解超时".into()));
            }
            tokio::time::sleep(Duration::from_secs(2)).await;

            let rurl = format!(
                "{}/result?id={}",
                self.base_url,
                urlencode(&accepted.task_id)
            );
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
                return Err(AppError::SolverFailed("求解任务过期/未知".into()));
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
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
                        return Err(AppError::SolverFailed("求解成功但 token 为空".into()));
                    }
                    return Ok(token);
                }
                "process" | "processing" => continue,
                "error" => {
                    return Err(AppError::SolverFailed(
                        sr.message.unwrap_or_else(|| "求解失败".into()),
                    ));
                }
                other => {
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
