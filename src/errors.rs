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
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = serde_json::json!({
            "error": {
                "message": self.to_string(),
                "type": self.error_type(),
                "code": self.error_type(),
            }
        });
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
}
