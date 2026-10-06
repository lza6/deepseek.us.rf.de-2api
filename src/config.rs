//! 配置：上游站点参数、求解器、监听地址、鉴权。
//!
//! 配置来源优先级：环境变量 > config.json > 内置默认。
//! 零硬编码密钥（api_keys 默认空 = 仅本机放行）。

use serde::{Deserialize, Serialize};
use std::path::Path;

fn default_listen() -> String {
    "127.0.0.1:47833".into()
}
fn default_upstream() -> String {
    "https://deepseek.es".into()
}
fn default_bot_id() -> String {
    "27623".into()
}
fn default_sitekey() -> String {
    "0x4AAAAAADlLZ3ljqZP6cQwq".into()
}
fn default_solver() -> String {
    "http://127.0.0.1:8001".into()
}
fn default_user_agent() -> String {
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36".into()
}
fn default_model() -> String {
    "deepseek-es".into()
}
fn default_solver_timeout() -> u64 {
    120
}
fn default_http_timeout() -> u64 {
    120
}
fn default_cookie_ttl() -> u64 {
    // 保守：上游 cookie 实际更长，30 分钟主动刷新
    1800
}
fn default_pseudo_chunk_chars() -> usize {
    0
}

/// 顶层配置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// 监听地址
    #[serde(default = "default_listen")]
    pub listen_addr: String,
    /// 上游站点基址
    #[serde(default = "default_upstream")]
    pub upstream_base_url: String,
    /// AIPKit bot id（聊天实例）
    #[serde(default = "default_bot_id")]
    pub bot_id: String,
    /// Cloudflare Turnstile sitekey
    #[serde(default = "default_sitekey")]
    pub sitekey: String,
    /// cf_solver 服务地址
    #[serde(default = "default_solver")]
    pub cf_solver_url: String,
    /// 浏览器 UA（cf_clearance/cookie 绑定，必须与求解一致）
    #[serde(default = "default_user_agent")]
    pub user_agent: String,
    /// 默认模型别名
    #[serde(default = "default_model")]
    pub default_model: String,
    /// 下游 API Key 白名单；空 = 仅本机放行（无鉴权）
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// 出口代理（可选，如 http://127.0.0.1:10808）
    #[serde(default)]
    pub proxy: Option<String>,
    /// 求解器超时（秒）
    #[serde(default = "default_solver_timeout")]
    pub solver_timeout_secs: u64,
    /// 上游 HTTP 超时（秒）
    #[serde(default = "default_http_timeout")]
    pub http_timeout_secs: u64,
    /// cookie 缓存 TTL（秒）
    #[serde(default = "default_cookie_ttl")]
    pub cookie_ttl_secs: u64,
    /// 伪流式分块字符数（0=关闭；上游已原生流式，通常无需）
    #[serde(default = "default_pseudo_chunk_chars")]
    pub pseudo_chunk_chars: usize,
    /// CORS 允许来源；空 = 关闭
    #[serde(default)]
    pub cors_allow_origins: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen_addr: default_listen(),
            upstream_base_url: default_upstream(),
            bot_id: default_bot_id(),
            sitekey: default_sitekey(),
            cf_solver_url: default_solver(),
            user_agent: default_user_agent(),
            default_model: default_model(),
            api_keys: vec![],
            proxy: None,
            solver_timeout_secs: default_solver_timeout(),
            http_timeout_secs: default_http_timeout(),
            cookie_ttl_secs: default_cookie_ttl(),
            pseudo_chunk_chars: default_pseudo_chunk_chars(),
            cors_allow_origins: vec![],
        }
    }
}

impl Config {
    /// 从文件加载（不存在则用默认），再叠加环境变量。
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        let mut cfg = if let Some(p) = path {
            if p.exists() {
                let text = std::fs::read_to_string(p)?;
                serde_json::from_str(&text)?
            } else {
                Config::default()
            }
        } else {
            Config::default()
        };
        cfg.apply_env();
        Ok(cfg)
    }

    fn apply_env(&mut self) {
        if let Ok(v) = std::env::var("LISTEN_ADDR") {
            if !v.trim().is_empty() {
                self.listen_addr = v;
            }
        }
        if let Ok(v) = std::env::var("UPSTREAM_BASE_URL") {
            if !v.trim().is_empty() {
                self.upstream_base_url = v;
            }
        }
        if let Ok(v) = std::env::var("BOT_ID") {
            if !v.trim().is_empty() {
                self.bot_id = v;
            }
        }
        if let Ok(v) = std::env::var("SITEKEY") {
            if !v.trim().is_empty() {
                self.sitekey = v;
            }
        }
        if let Ok(v) = std::env::var("CF_SOLVER_URL") {
            if !v.trim().is_empty() {
                self.cf_solver_url = v;
            }
        }
        if let Ok(v) = std::env::var("USER_AGENT") {
            if !v.trim().is_empty() {
                self.user_agent = v;
            }
        }
        if let Ok(v) = std::env::var("DEFAULT_MODEL") {
            if !v.trim().is_empty() {
                self.default_model = v;
            }
        }
        if let Ok(v) = std::env::var("PROXY") {
            if !v.trim().is_empty() {
                self.proxy = Some(v);
            }
        }
        if let Ok(v) = std::env::var("API_KEYS") {
            let keys: Vec<String> = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !keys.is_empty() {
                self.api_keys = keys;
            }
        }
        if let Ok(v) = std::env::var("CORS_ALLOW_ORIGINS") {
            let origins: Vec<String> = v
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            self.cors_allow_origins = origins;
        }
    }

    /// 上游 AJAX 端点
    pub fn ajax_url(&self) -> String {
        format!(
            "{}/wp-admin/admin-ajax.php",
            self.upstream_base_url.trim_end_matches('/')
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = Config::default();
        assert_eq!(c.bot_id, "27623");
        assert_eq!(c.sitekey, "0x4AAAAAADlLZ3ljqZP6cQwq");
        assert!(c.api_keys.is_empty());
        assert_eq!(c.ajax_url(), "https://deepseek.es/wp-admin/admin-ajax.php");
    }

    #[test]
    fn env_override_works() {
        std::env::set_var("BOT_ID", "99999");
        let c = Config::default();
        let mut c2 = c.clone();
        c2.apply_env();
        assert_eq!(c2.bot_id, "99999");
        std::env::remove_var("BOT_ID");
    }
}
