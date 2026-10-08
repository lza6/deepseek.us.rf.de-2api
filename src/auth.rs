//! 下游鉴权：Bearer token / x-api-key 白名单；空白名单 = 仅本机放行。

use crate::config::Config;
use crate::errors::{AppError, AppResult};
use axum::http::HeaderMap;

/// 校验下游请求。返回 Ok(()) 表示放行。
///
/// 规则：
/// - 配置了 `api_keys`：必须匹配其中一个（`Authorization: Bearer <k>` 或 `x-api-key: <k>`）；
///   支持过期时间（见 `ApiKey`）。
/// - 未配置 `api_keys`：仅允许本机（由启动时 `validate_security` + 监听地址保证），此处恒放行。
///
/// **常量时间**：对所有候选 key 逐一比较并**累积**结果（不短路），
/// 避免通过响应时间泄漏「匹配到了第几个 key」或「前缀是否命中」。
pub fn check_auth(cfg: &Config, headers: &HeaderMap) -> AppResult<()> {
    if cfg.api_keys.is_empty() {
        return Ok(());
    }
    let Some(provided) = extract_key(headers) else {
        return Err(AppError::Unauthorized);
    };
    let now = chrono::Utc::now().timestamp();
    let mut matched = false;
    for entry in cfg.api_keys.iter() {
        // 过期 key 直接跳过（先判过期再比较，避免对已失效 key 做无谓比较；
        // 过期与否不是秘密，不构成时序泄漏面）。
        if entry.is_expired(now) {
            continue;
        }
        // 不短路：即使已匹配也继续比较其余 key（保持总比较次数与配置项数相关而与命中位置无关）。
        matched |= constant_eq(entry.secret(), &provided);
    }
    if matched {
        Ok(())
    } else {
        Err(AppError::Unauthorized)
    }
}

fn extract_key(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        if !v.trim().is_empty() {
            return Some(v.trim().to_string());
        }
    }
    if let Some(v) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let v = v.trim();
        if let Some(rest) = v.strip_prefix("Bearer ") {
            return Some(rest.trim().to_string());
        }
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    None
}

/// 常量时间比较（防时序侧信道）。
fn constant_eq(a: &str, b: &str) -> bool {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    if ab.len() != bb.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..ab.len() {
        diff |= ab[i] ^ bb[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn cfg_with_keys(keys: &[&str]) -> Config {
        Config {
            api_keys: keys
                .iter()
                .map(|s| crate::config::ApiKey::plain(*s))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn empty_keys_allows_all() {
        let cfg = Config::default();
        assert!(check_auth(&cfg, &HeaderMap::new()).is_ok());
    }

    #[test]
    fn bearer_ok() {
        let cfg = cfg_with_keys(&["sk-test"]);
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("Bearer sk-test"));
        assert!(check_auth(&cfg, &h).is_ok());
    }

    #[test]
    fn x_api_key_ok() {
        let cfg = cfg_with_keys(&["sk-test"]);
        let mut h = HeaderMap::new();
        h.insert("x-api-key", HeaderValue::from_static("sk-test"));
        assert!(check_auth(&cfg, &h).is_ok());
    }

    #[test]
    fn wrong_key_rejected() {
        let cfg = cfg_with_keys(&["sk-test"]);
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("Bearer sk-wrong"));
        assert!(check_auth(&cfg, &h).is_err());
    }

    #[test]
    fn missing_key_rejected() {
        let cfg = cfg_with_keys(&["sk-test"]);
        assert!(check_auth(&cfg, &HeaderMap::new()).is_err());
    }

    #[test]
    fn constant_eq_works() {
        assert!(constant_eq("abc", "abc"));
        assert!(!constant_eq("abc", "abd"));
        assert!(!constant_eq("abc", "ab"));
    }
}
