//! 统一错误类型与 HTTP 映射。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("请求参数非法: {0}")]
    BadRequest(String),

    #[error("未授权")]
    Unauthorized,

    #[error("上游安全校验要求重新求解 Turnstile")]
    TsRequired,

    #[error("Turnstile 求解失败: {0}")]
    SolverFailed(String),

    #[error("上游配额耗尽: {0}")]
    QuotaExhausted(String),

    #[error("上游错误: {0}")]
    Upstream(String),

    #[error("上游流错误: {0}")]
    UpstreamStream(String),

    #[error("网络错误: {0}")]
    Network(String),

    #[error("内部错误: {0}")]
    Internal(String),
}

impl AppError {
    /// OpenAI 风格错误类型字符串
    pub fn error_type(&self) -> &'static str {
        match self {
            AppError::BadRequest(_) => "invalid_request_error",
            AppError::Unauthorized => "authentication_error",
            AppError::TsRequired | AppError::SolverFailed(_) => "api_error",
            AppError::QuotaExhausted(_) => "rate_limit_error",
            AppError::Upstream(_) | AppError::UpstreamStream(_) => "upstream_error",
            AppError::Network(_) => "network_error",
            AppError::Internal(_) => "internal_error",
        }
    }

    /// L10：更细的机器可读错误码（区别于粗分类的 `type`）。
    ///
    /// OpenAI 规范中 `code` 常为 null 或更具体的标识；此前与 `type` 恒相同。
    pub fn error_code(&self) -> Option<&'static str> {
        Some(match self {
            AppError::BadRequest(_) => "invalid_request",
            AppError::Unauthorized => "invalid_api_key",
            AppError::TsRequired => "ts_required",
            AppError::SolverFailed(_) => "solver_failed",
            AppError::QuotaExhausted(_) => "quota_exhausted",
            AppError::Upstream(_) => "upstream_error",
            AppError::UpstreamStream(_) => "upstream_stream_error",
            AppError::Network(_) => "network_error",
            AppError::Internal(_) => "internal_error",
        })
    }

    /// Anthropic 风格错误类型字符串。
    ///
    /// Anthropic 的合法取值：`invalid_request_error` / `authentication_error` /
    /// `permission_error` / `not_found_error` / `rate_limit_error` / `api_error` /
    /// `overloaded_error`。**没有** `upstream_error`/`network_error`/`internal_error`，
    /// 故这些统一映射为 `api_error`（否则 Anthropic SDK 无法识别）。
    pub fn anthropic_error_type(&self) -> &'static str {
        match self {
            AppError::BadRequest(_) => "invalid_request_error",
            AppError::Unauthorized => "authentication_error",
            AppError::QuotaExhausted(_) => "rate_limit_error",
            _ => "api_error",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::QuotaExhausted(_) => StatusCode::TOO_MANY_REQUESTS,
            AppError::TsRequired | AppError::SolverFailed(_) => StatusCode::BAD_GATEWAY,
            AppError::Upstream(_) | AppError::UpstreamStream(_) => StatusCode::BAD_GATEWAY,
            AppError::Network(_) => StatusCode::BAD_GATEWAY,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// 转成 Anthropic 兼容错误响应体（H3 修复）。
    ///
    /// Anthropic 规范要求顶层 `{"type":"error","error":{"type":..,"message":..}}`，
    /// 而非 OpenAI 的 `{"error":{...}}`。若返回 OpenAI 结构，Claude Code / Anthropic SDK
    /// 解析错误时会失败。
    pub fn into_anthropic_response(self) -> Response {
        let status = self.status();
        let body = serde_json::json!({
            "type": "error",
            "error": {
                "type": self.anthropic_error_type(),
                "message": self.to_string(),
            }
        });
        (status, Json(body)).into_response()
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        // L10：`code` 用更细的机器码（OpenAI 中 `type` 是粗分类、`code` 是细分码），
        // 此前二者恒相同。`code` 为 None 时按规范可省略。
        let code = self.error_code();
        let mut err = serde_json::json!({
            "message": self.to_string(),
            "type": self.error_type(),
        });
        if let Some(c) = code {
            err["code"] = serde_json::Value::String(c.to_string());
        }
        let body = serde_json::json!({ "error": err });
        (status, Json(body)).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping() {
        assert_eq!(
            AppError::BadRequest("x".into()).status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(AppError::Unauthorized.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            AppError::QuotaExhausted("q".into()).status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(AppError::TsRequired.status(), StatusCode::BAD_GATEWAY);
    }

    #[test]
    fn error_type_mapping() {
        assert_eq!(AppError::Unauthorized.error_type(), "authentication_error");
        assert_eq!(
            AppError::QuotaExhausted("q".into()).error_type(),
            "rate_limit_error"
        );
    }

    // ── H3 回归：Anthropic 错误类型必须是合法取值 ────────────────

    #[test]
    fn anthropic_error_type_only_uses_valid_values() {
        const VALID: &[&str] = &[
            "invalid_request_error",
            "authentication_error",
            "permission_error",
            "not_found_error",
            "rate_limit_error",
            "api_error",
            "overloaded_error",
        ];
        let all = [
            AppError::BadRequest("x".into()),
            AppError::Unauthorized,
            AppError::TsRequired,
            AppError::SolverFailed("s".into()),
            AppError::QuotaExhausted("q".into()),
            AppError::Upstream("u".into()),
            AppError::UpstreamStream("us".into()),
            AppError::Network("n".into()),
            AppError::Internal("i".into()),
        ];
        for e in &all {
            let t = e.anthropic_error_type();
            assert!(
                VALID.contains(&t),
                "非法 Anthropic 错误类型: {t} (from {e:?})"
            );
        }
        // 非 Anthropic 原生的类型必须收敛到 api_error
        assert_eq!(
            AppError::Upstream("u".into()).anthropic_error_type(),
            "api_error"
        );
        assert_eq!(
            AppError::Network("n".into()).anthropic_error_type(),
            "api_error"
        );
        assert_eq!(
            AppError::Internal("i".into()).anthropic_error_type(),
            "api_error"
        );
    }
}
