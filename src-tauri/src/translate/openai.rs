//! OpenAI 兼容接口。
//!
//! 一套实现覆盖一大批服务——OpenAI、DeepSeek、Kimi、智谱、通义、OpenRouter、
//! Groq，以及 Ollama / LM Studio 这类本地服务，区别只在 base_url 和 model。
//! 这是「多加几个主流引擎」性价比最高的做法：与其为每家写一个模块，
//! 不如认准这个事实标准。

use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::json;

use super::{language_name, Translation, HTTP};
use crate::config;

#[derive(Deserialize)]
struct ChatResponse {
    choices: Option<Vec<Choice>>,
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct Choice {
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ApiError {
    message: Option<String>,
}

pub async fn translate(text: &str, source: &str, target: &str) -> Result<Translation> {
    let cfg = config::get();
    let key = cfg.openai_api_key.trim();
    // 本地服务（Ollama / LM Studio）通常不校验 key，所以只在非本地时强制要求
    let base = cfg.openai_base_url.trim().trim_end_matches('/');
    let local = base.contains("localhost") || base.contains("127.0.0.1");
    if key.is_empty() && !local {
        return Err(anyhow!("未配置 API Key，请到设置里填写。"));
    }
    if base.is_empty() {
        return Err(anyhow!("未配置接口地址，请到设置里填写。"));
    }

    let model = if cfg.openai_model.trim().is_empty() {
        "gpt-4o-mini"
    } else {
        cfg.openai_model.trim()
    };

    let from = if source == "auto" || source.is_empty() {
        String::new()
    } else {
        format!("The source text is in {}. ", language_name(source))
    };

    let system = format!(
        "You are a translation engine. {from}Translate the user's text into {}. \
         Output only the translation. No preamble, no explanation, no surrounding quotes. \
         Preserve the original line breaks, lists, and code blocks. \
         If a passage is already in the target language, return it unchanged.",
        language_name(target)
    );

    let mut request = HTTP.post(format!("{base}/chat/completions"));
    if !key.is_empty() {
        request = request.header("Authorization", format!("Bearer {key}"));
    }

    let response = request
        .json(&json!({
            "model": model,
            "temperature": 0.2,
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": text },
            ],
        }))
        .send()
        .await?;

    let status = response.status();
    let parsed: ChatResponse = response.json().await.map_err(|e| {
        anyhow!("接口返回了无法解析的内容（HTTP {}）：{e}", status.as_u16())
    })?;

    if let Some(err) = parsed.error.and_then(|e| e.message) {
        return Err(anyhow!("接口报错：{err}"));
    }

    let translated = parsed
        .choices
        .and_then(|c| c.into_iter().next())
        .and_then(|c| c.message)
        .and_then(|m| m.content)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("接口返回了空内容（HTTP {}）", status.as_u16()))?;

    Ok(Translation {
        text: translated,
        provider: format!("OpenAI 兼容 · {model}"),
        target: target.to_string(),
        detected_source: None,
    })
}
