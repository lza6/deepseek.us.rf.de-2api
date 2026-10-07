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
fn default_solver_retries() -> u32 {
    2
}
fn default_breaker_threshold() -> u32 {
    5
}
fn default_breaker_cooldown() -> u64 {
    30
}
fn default_max_concurrency() -> usize {
    0
}
fn default_rate_limit() -> u64 {
    0
}
fn default_false() -> bool {
    false
}
fn default_cache_ttl() -> u64 {
    300
}
fn default_cache_max() -> usize {
    1000
}
fn default_admin_token() -> String {
    String::new()
}
fn default_ledger_path() -> String {
    "usage.db".into()
}
fn default_solvers() -> Vec<String> {
    vec![]
}
fn default_language_prompt() -> String {
    String::new()
}
fn default_cache_max_bytes() -> usize {
    2 * 1024 * 1024
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
    /// CORS 允许来源；空 = 关闭
    #[serde(default)]
    pub cors_allow_origins: Vec<String>,
    /// 求解失败重试次数（不含首次；0=不重试）
    #[serde(default = "default_solver_retries")]
    pub solver_retries: u32,
    /// 认证熔断：连续失败阈值（达到后打开熔断）
    #[serde(default = "default_breaker_threshold")]
    pub breaker_fail_threshold: u32,
    /// 认证熔断：打开后的冷却秒数（冷却后半开重试）
    #[serde(default = "default_breaker_cooldown")]
    pub breaker_cooldown_secs: u64,
    /// 最大并发请求数（0=不限）
    #[serde(default = "default_max_concurrency")]
    pub max_concurrency: usize,
    /// 每秒请求上限（0=不限）
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_sec: u64,
    /// 控制台 Web UI 开关（`/admin`）
    #[serde(default = "default_false")]
    pub admin_enabled: bool,
    /// 控制台访问令牌（空 = 未设置，控制台拒绝访问并提示）
    #[serde(default = "default_admin_token")]
    pub admin_token: String,
    /// 请求级响应缓存：TTL 秒（0=关闭）
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_secs: u64,
    /// 请求级响应缓存：最大条目数
    #[serde(default = "default_cache_max")]
    pub cache_max_entries: usize,
    /// 请求级响应缓存：命中下限（prompt+回复 字符数，低于不缓存，避免噪声）
    #[serde(default)]
    pub cache_min_chars: usize,
    /// 用量账本：SQLite 路径（空 = 关闭持久化）
    #[serde(default = "default_ledger_path")]
    pub ledger_path: String,
    /// 多 cf_solver 实例（空 = 使用 `cf_solver_url` 单实例）
    #[serde(default = "default_solvers")]
    pub solver_urls: Vec<String>,
    /// 提示词注入：追加到 system 的语言指令（空 = 不注入）
    #[serde(default = "default_language_prompt")]
    pub system_prompt_suffix: String,
    /// 非流式聚合输出上限（字节；超限返回错误，防上游超大响应）
    #[serde(default = "default_cache_max_bytes")]
    pub max_response_bytes: usize,
    /// 伪工具调用开关（H4）：启用后在 prompt 注入工具说明，使模型知晓可发 ` ```tool ` 块
    #[serde(default = "default_false")]
    pub pseudo_tools_enabled: bool,
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
            cors_allow_origins: vec![],
            solver_retries: default_solver_retries(),
            breaker_fail_threshold: default_breaker_threshold(),
            breaker_cooldown_secs: default_breaker_cooldown(),
            max_concurrency: default_max_concurrency(),
            rate_limit_per_sec: default_rate_limit(),
            admin_enabled: false,
            admin_token: default_admin_token(),
            cache_ttl_secs: default_cache_ttl(),
            cache_max_entries: default_cache_max(),
            cache_min_chars: 0,
            ledger_path: default_ledger_path(),
            solver_urls: default_solvers(),
            system_prompt_suffix: default_language_prompt(),
            max_response_bytes: default_cache_max_bytes(),
            pseudo_tools_enabled: false,
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

    /// 安全校验：非回环监听且未配置 api_keys 时拒绝启动（除非显式放开）。
    ///
    /// 背景：空 `api_keys` 的语义是"仅本机放行"（见 `auth.rs`）。
    /// 若监听地址被改为 `0.0.0.0`（如 Dockerfile），该前提被破坏，
    /// 网关会变成无鉴权的开放代理。这里 fail-fast 阻止误用。
    ///
    /// 逃生阀：环境变量 `ALLOW_INSECURE_PUBLIC=1`（仅供深知风险者）。
    pub fn validate_security(&self) -> anyhow::Result<()> {
        let allow_insecure = std::env::var("ALLOW_INSECURE_PUBLIC")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        self.validate_security_with(allow_insecure)
    }

    fn validate_security_with(&self, allow_insecure: bool) -> anyhow::Result<()> {
        if allow_insecure || !self.api_keys.is_empty() {
            return Ok(());
        }
        if host_is_loopback(&self.listen_addr) {
            return Ok(());
        }
        anyhow::bail!(
            "拒绝启动：监听地址 {} 非本机回环，但未配置 api_keys，网关将无鉴权暴露。\n\
             请设置环境变量 API_KEYS（或 config.json 的 api_keys）；\n\
             确需无鉴权暴露公网时，显式设置 ALLOW_INSECURE_PUBLIC=1（危险，不推荐）。",
            self.listen_addr
        )
    }
}

/// 从 `host:port` 提取 host（兼容 `[::1]:port` 形式）。
fn listen_host(addr: &str) -> &str {
    if let Some(rest) = addr.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return &rest[..end];
        }
    }
    addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr)
}

/// 判断监听地址是否为回环（`localhost` / 127.0.0.0/8 / ::1）。
/// 无法解析的 host 一律视为非回环（保守拒绝）。
fn host_is_loopback(addr: &str) -> bool {
    let host = listen_host(addr);
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .map(|ip| ip.is_loopback())
        .unwrap_or(false)
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

    #[test]
    fn reject_public_bind_without_keys() {
        // 0.0.0.0 + 空 keys → 拒绝启动
        let c = Config {
            listen_addr: "0.0.0.0:47833".into(),
            api_keys: vec![],
            ..Default::default()
        };
        assert!(c.validate_security_with(false).is_err());
    }

    #[test]
    fn allow_public_bind_with_keys() {
        // 0.0.0.0 + 有 keys → 放行
        let c = Config {
            listen_addr: "0.0.0.0:47833".into(),
            api_keys: vec!["sk-x".into()],
            ..Default::default()
        };
        assert!(c.validate_security_with(false).is_ok());
    }

    #[test]
    fn allow_loopback_without_keys() {
        // 回环 + 空 keys → 放行（本机语义）
        for addr in ["127.0.0.1:47833", "localhost:47833", "[::1]:47833"] {
            let c = Config {
                listen_addr: addr.into(),
                api_keys: vec![],
                ..Default::default()
            };
            assert!(
                c.validate_security_with(false).is_ok(),
                "{addr} 应为回环放行"
            );
        }
    }

    #[test]
    fn explicit_escape_hatch_allows_public() {
        // 显式逃生阀：0.0.0.0 + 空 keys + allow_insecure → 放行
        let c = Config {
            listen_addr: "0.0.0.0:47833".into(),
            api_keys: vec![],
            ..Default::default()
        };
        assert!(c.validate_security_with(true).is_ok());
    }

    #[test]
    fn loopback_detection_helper() {
        assert!(host_is_loopback("127.0.0.1:1"));
        assert!(host_is_loopback("127.5.5.5:1"));
        assert!(host_is_loopback("localhost:1"));
        assert!(host_is_loopback("[::1]:1"));
        assert!(!host_is_loopback("0.0.0.0:1"));
        assert!(!host_is_loopback("192.168.1.1:1"));
        assert!(!host_is_loopback("example.com:1"));
        assert!(!host_is_loopback(""));
    }
}
