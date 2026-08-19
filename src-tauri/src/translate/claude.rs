use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use super::{language_name, Translation, HTTP};
use crate::config;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";

#[derive(Deserialize)]
struct ClaudeResponse {
    content: Option<Vec<ContentBlock>>,
    stop_reason: Option<String>,
    stop_details: Option<StopDetails>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct StopDetails {
    category: Option<String>,
}

/// 通用的一次性 Claude 调用。
///
/// 划词弹窗对延迟很敏感，所以这里关掉 thinking 并把 effort 压到 low。
/// 关掉 thinking 时官方建议显式禁止模型输出内部 XML 标签，否则偶尔会漏出来。
pub async fn ask(system: &str, user: &str, max_tokens: u32) -> Result<String> {
    let cfg = config::get();
    let key = cfg.claude_api_key.trim();
    if key.is_empty() {
        return Err(anyhow!("未配置 Claude API Key，请到设置里填写。"));
    }

    let model = if cfg.claude_model.trim().is_empty() {
        "claude-opus-5".to_string()
    } else {
        cfg.claude_model.trim().to_string()
    };

    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "messages": [{ "role": "user", "content": user }],
    });

    // 新一代模型支持 effort；Fable / Mythos 的 thinking 常开，显式关闭会 400
    let modern = [
        "claude-opus-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-5",
        "claude-sonnet-4-6",
        "claude-fable-5",
        "claude-mythos-5",
    ]
    .iter()
    .any(|prefix| model.starts_with(prefix));
    let always_thinks = model.contains("fable") || model.contains("mythos");

    if modern {
        body["output_config"] = json!({ "effort": "low" });
        if !always_thinks {
            body["thinking"] = json!({ "type": "disabled" });
        }
    }

    let response = HTTP
        .post(API_URL)
        .header("content-type", "application/json")
        .header("x-api-key", key)
        .header("anthropic-version", API_VERSION)
        .json(&body)
        .send()
        .await?;

    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        let detail: String = detail.chars().take(400).collect();
        return Err(anyhow!("Claude 请求失败（HTTP {}）：{detail}", status.as_u16()));
    }

    let parsed: ClaudeResponse = response.json().await?;

    // 先看 stop_reason 再读 content —— 被安全策略拒绝时 content 可能是空的
    if parsed.stop_reason.as_deref() == Some("refusal") {
        let category = parsed
            .stop_details
            .and_then(|d| d.category)
            .unwrap_or_else(|| "未知".into());
        return Err(anyhow!("Claude 拒绝了这次请求（类别：{category}）"));
    }

    let text: String = parsed
        .content
        .unwrap_or_default()
        .into_iter()
        .filter(|b| b.kind == "text")
        .filter_map(|b| b.text)
        .collect::<Vec<_>>()
        .join("");

    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        return Err(anyhow!("Claude 返回了空内容"));
    }
    Ok(trimmed)
}

pub async fn translate(text: &str, source: &str, target: &str) -> Result<Translation> {
    // 用户指定了源语言就写进提示词——短词歧义（"die" 是英语还是德语）
    // 这类场合，明确告知比让模型自己猜可靠
    let from = if source == "auto" || source.is_empty() {
        String::new()
    } else {
        format!("The source text is in {}. ", language_name(source))
    };

    let system = format!(
        "You are a translation engine. {from}Translate the user's text into {}. \
         Output only the translation. No preamble, no explanation, no surrounding quotes. \
         Preserve the original line breaks, lists, and code blocks. \
         If a passage is already in the target language, return it unchanged. \
         Do not include internal or system XML tags in your response.",
        language_name(target)
    );

    let translated = ask(&system, text, 8192).await?;

    Ok(Translation {
        text: translated,
        provider: format!("Claude · {}", config::get().claude_model),
        target: target.to_string(),
        detected_source: None,
    })
}

/// 给「AI 内容提取」留的口子（当前版本前端未启用，保留供后续开关）
#[allow(dead_code)]
pub async fn extract(text: &str, instruction: &str) -> Result<Value> {
    let output = ask(instruction, text, 4096).await?;
    Ok(json!({ "text": output }))
}
