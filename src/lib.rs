//! deepseek.es → OpenAI/Anthropic 兼容 API 网关（lib）。
//!
//! 模块：
//! - `config`   配置加载
//! - `errors`   统一错误
//! - `models`   模型目录
//! - `solver`   Cloudflare Turnstile 求解客户端（对接 cf_solver）
//! - `upstream` 上游 deepseek.es 客户端（认证/nonce/cache/SSE）
//! - `session`  会话映射
//! - `auth`     下游鉴权
//! - `api`      axum 路由与处理器
//! - `protocol` OpenAI/Anthropic 协议翻译

pub mod api;
pub mod auth;
pub mod config;
pub mod errors;
pub mod models;
pub mod protocol;
pub mod session;
pub mod solver;
pub mod upstream;

pub use config::Config;
pub use errors::{AppError, AppResult};
pub use upstream::UpstreamClient;
