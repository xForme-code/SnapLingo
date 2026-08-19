use anyhow::{anyhow, Result};
use serde::Deserialize;

use super::{Translation, HTTP};
use crate::config;

#[derive(Deserialize)]
struct DeepLResponse {
    translations: Option<Vec<DeepLItem>>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct DeepLItem {
    text: String,
    detected_source_language: Option<String>,
}

/// 内部语言代码 → DeepL 的代码
fn to_deepl_lang(code: &str) -> String {
    match code {
        "zh-CN" => "ZH-HANS",
        "zh-TW" => "ZH-HANT",
        "en" => "EN-US",
        "pt" => "PT-PT",
        other => return other.to_uppercase(),
    }
    .to_string()
}

/// 源语言代码 → DeepL 的 source_lang。返回 None 表示交给它自动识别。
///
/// 和 target_lang 不同：源语言不支持 ZH-HANS / EN-US 这类带地区的变体，
/// 传过去会 400，必须退回到基础代码。
fn to_deepl_source(code: &str) -> Option<String> {
    if code == "auto" || code.is_empty() {
        return None;
    }
    let base = code.split('-').next().unwrap_or(code);
    Some(base.to_uppercase())
}

pub async fn translate(text: &str, source: &str, target: &str) -> Result<Translation> {
    let cfg = config::get();
    let key = cfg.deepl_api_key.trim();
    if key.is_empty() {
        return Err(anyhow!("未配置 DeepL API Key，请到设置里填写。"));
    }

    let host = if cfg.deepl_pro {
        "https://api.deepl.com"
    } else {
        "https://api-free.deepl.com"
    };

    let target_lang = to_deepl_lang(target);
    let mut form = vec![("text", text), ("target_lang", target_lang.as_str())];

    // 不传 source_lang 就是让 DeepL 自己识别。
    // 注意源语言不接受 ZH-HANS 这种带地区的写法，只能是 ZH。
    let source_lang = to_deepl_source(source);
    if let Some(code) = source_lang.as_deref() {
        form.push(("source_lang", code));
    }

    let response = HTTP
        .post(format!("{host}/v2/translate"))
        .header("Authorization", format!("DeepL-Auth-Key {key}"))
        .form(&form)
        .send()
        .await?;

    let status = response.status();
    let parsed: DeepLResponse = response.json().await.map_err(|e| {
        anyhow!("DeepL 返回了无法解析的内容（HTTP {}）：{e}", status.as_u16())
    })?;

    if let Some(message) = parsed.message {
        return Err(anyhow!("DeepL 报错（HTTP {}）：{message}", status.as_u16()));
    }

    let first = parsed
        .translations
        .and_then(|mut list| if list.is_empty() { None } else { Some(list.remove(0)) })
        .ok_or_else(|| anyhow!("DeepL 返回了空结果"))?;

    Ok(Translation {
        text: first.text,
        provider: "DeepL".into(),
        target: target.to_string(),
        detected_source: first.detected_source_language,
    })
}
