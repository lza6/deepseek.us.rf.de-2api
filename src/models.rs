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
}

pub const DEFAULT_MODEL: &str = "deepseek-es";

/// 静态目录。provider 来自 AIPKit 枚举 + 实测 DeepSeek。
///
/// ⚠️ 这些是**路由别名**，映射到上游 bot 的 provider。真实模型 ID 由服务端决定。
pub fn catalog() -> Vec<ModelMeta> {
    vec![
        ModelMeta {
            id: "deepseek-es".into(),
            label: "DeepSeek ES (default)".into(),
            family: "DeepSeek".into(),
            context: "64k".into(),
            context_window: 65536,
            provider: "DeepSeek".into(),
            default: true,
            tools: false,
            vision: false,
            reasoning: true,
            web_search: false,
            google_grounding: false,
        },
        ModelMeta {
            id: "deepseek-es-openai".into(),
            label: "DeepSeek ES → OpenAI".into(),
            family: "OpenAI".into(),
            context: "128k".into(),
            context_window: 131072,
            provider: "OpenAI".into(),
            default: false,
            tools: false,
            vision: false,
            reasoning: false,
            web_search: true,
            google_grounding: false,
        },
        ModelMeta {
            id: "deepseek-es-claude".into(),
            label: "DeepSeek ES → Claude".into(),
            family: "Anthropic".into(),
            context: "200k".into(),
            context_window: 200000,
            provider: "Claude".into(),
            default: false,
            tools: false,
            vision: false,
            reasoning: true,
            web_search: true,
            google_grounding: false,
        },
        ModelMeta {
            id: "deepseek-es-google".into(),
            label: "DeepSeek ES → Gemini".into(),
            family: "Google".into(),
            context: "1M".into(),
            context_window: 1048576,
            provider: "Google".into(),
            default: false,
            tools: false,
            vision: false,
            reasoning: false,
            web_search: false,
            google_grounding: true,
        },
        ModelMeta {
            id: "deepseek-es-openrouter".into(),
            label: "DeepSeek ES → OpenRouter".into(),
            family: "OpenRouter".into(),
            context: "128k".into(),
            context_window: 131072,
            provider: "OpenRouter".into(),
            default: false,
            tools: false,
            vision: false,
            reasoning: false,
            web_search: true,
            google_grounding: false,
        },
        ModelMeta {
            id: "deepseek-es-xai".into(),
            label: "DeepSeek ES → xAI Grok".into(),
            family: "xAI".into(),
            context: "128k".into(),
            context_window: 131072,
            provider: "xAI".into(),
            default: false,
            tools: false,
            vision: false,
            reasoning: false,
            web_search: true,
            google_grounding: false,
        },
    ]
}

/// 解析下游请求的 model → 上游 provider。
/// 未知模型回退默认（对齐上游"未知 id 静默默认"行为，但这里显式回退）。
pub fn resolve_model(model: &str) -> ModelMeta {
    let m = model.trim();
    if m.is_empty() {
        return catalog().into_iter().find(|c| c.default).unwrap();
    }
    catalog()
        .into_iter()
        .find(|c| c.id == m)
        .unwrap_or_else(|| {
            // 允许直接传 provider 名或 deepseek 家族名
            let lower = m.to_lowercase();
            if let Some(found) = catalog()
                .into_iter()
                .find(|c| c.provider.to_lowercase() == lower)
            {
                return found;
            }
            if lower.contains("deepseek") || lower == "default" {
                return catalog().into_iter().find(|c| c.default).unwrap();
            }
            catalog().into_iter().find(|c| c.default).unwrap()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_not_empty() {
        assert!(!catalog().is_empty());
        assert_eq!(catalog().iter().filter(|m| m.default).count(), 1);
    }

    #[test]
    fn resolve_default() {
        assert_eq!(resolve_model("").id, "deepseek-es");
        assert_eq!(resolve_model("deepseek-es").id, "deepseek-es");
    }

    #[test]
    fn resolve_unknown_falls_back() {
        assert_eq!(resolve_model("gpt-4o").id, "deepseek-es");
        assert_eq!(resolve_model("deepseek-chat").id, "deepseek-es");
    }

    #[test]
    fn resolve_by_provider_name() {
        assert_eq!(resolve_model("openai").id, "deepseek-es-openai");
        assert_eq!(resolve_model("Claude").id, "deepseek-es-claude");
    }
}
