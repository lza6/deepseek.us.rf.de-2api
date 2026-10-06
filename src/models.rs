//! 模型目录：deepseek.es 前端不暴露真实模型 ID，仅暴露 provider。
//!
//! E2E 实测：本站 provider=`DeepSeek`（AIPKit 隐藏扩展），botId=27623。
//! 由于上游无标准模型列表端点，这里暴露 provider 级别的模型别名。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelMeta {
    pub id: String,
    pub label: String,
    pub family: String,
    pub context: String,
    pub context_window: i64,
    pub provider: String,
    /// 是否本站默认（botId 对应的上游配置）
    pub default: bool,
    /// 能力标记
    pub tools: bool,
    pub vision: bool,
    pub reasoning: bool,
    /// 是否支持联网搜索
    pub web_search: bool,
    /// 是否支持 Google 搜索接地
    pub google_grounding: bool,
    /// 若为别名，指向真实模型 id；真实模型为 None
    pub alias_of: Option<String>,
    /// 是否可真实路由到上游独立 bot（本站仅单 bot，故仅默认模型为 true）
    pub routable: bool,
}

pub const DEFAULT_MODEL: &str = "deepseek-es";

/// 历史遗留别名（曾宣称"按 provider 路由"，实为单一上游 bot 的等价别名）。
/// 这些字符串仍被 `resolve_model` 接受以兼容旧客户端，但**不再出现在** `/v1/models`。
pub const LEGACY_ALIASES: &[&str] = &[
    "deepseek-es-openai",
    "deepseek-es-claude",
    "deepseek-es-google",
    "deepseek-es-openrouter",
    "deepseek-es-xai",
];

/// 唯一的真实模型。
///
/// **实测**（`分析文档/05` §0）：本站只有一个 bot（`bot_id=27623`），
/// `provider` 由服务端固定为 `DeepSeek`，前端不暴露模型 ID，代理**无法切换** provider。
/// 因此只暴露这一个真实可路由的模型（诚实化，替代此前 6 个虚构别名）。
pub fn catalog() -> Vec<ModelMeta> {
    vec![ModelMeta {
        id: DEFAULT_MODEL.to_string(),
        label: "DeepSeek ES".to_string(),
        family: "DeepSeek".to_string(),
        context: "64k".to_string(),
        context_window: 65536,
        provider: "DeepSeek".to_string(),
        default: true,
        // 上游实测均不支持（分析文档/05 §0：工具/联网/上传全 false）
        tools: false,
        vision: false,
        reasoning: true,
        web_search: false,
        google_grounding: false,
        alias_of: None,
        routable: true,
    }]
}

/// 解析下游请求的 `model` → 真实模型。
///
/// 本站为单一模型：任何 id（含历史别名、provider 名、任意字符串）均回退到默认模型，
/// 但会记录 debug 日志以便排查。响应中的 `model` 字段仍回显请求值（对齐 OpenAI 行为）。
pub fn resolve_model(model: &str) -> ModelMeta {
    let base = catalog().into_iter().next().expect("catalog 非空");
    let m = model.trim();
    if !m.is_empty() && !m.eq_ignore_ascii_case(&base.id) && m.to_lowercase() != "default" {
        if LEGACY_ALIASES.iter().any(|a| a.eq_ignore_ascii_case(m)) {
            tracing::debug!(
                "模型 '{m}' 为历史别名，等价于 '{}'（不再路由到独立模型）",
                base.id
            );
        } else {
            tracing::debug!("模型 '{m}' 非本站模型，回退默认 '{}'", base.id);
        }
    }
    base
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_single_routable_model() {
        let c = catalog();
        assert_eq!(c.len(), 1, "本站仅单一真实模型");
        assert_eq!(c[0].id, "deepseek-es");
        assert!(c[0].default);
        assert!(c[0].routable);
        assert!(c[0].alias_of.is_none());
    }

    #[test]
    fn resolve_default() {
        assert_eq!(resolve_model("").id, "deepseek-es");
        assert_eq!(resolve_model("deepseek-es").id, "deepseek-es");
        assert_eq!(resolve_model("  deepseek-es  ").id, "deepseek-es");
    }

    #[test]
    fn resolve_unknown_falls_back() {
        assert_eq!(resolve_model("gpt-4o").id, "deepseek-es");
        assert_eq!(resolve_model("deepseek-chat").id, "deepseek-es");
        assert_eq!(resolve_model("openai").id, "deepseek-es");
        assert_eq!(resolve_model("Claude").id, "deepseek-es");
    }

    #[test]
    fn legacy_aliases_still_accepted() {
        // 历史别名不再列出，但仍解析到默认模型（兼容旧客户端）
        for alias in LEGACY_ALIASES {
            assert_eq!(resolve_model(alias).id, "deepseek-es", "alias={alias}");
        }
    }
}
