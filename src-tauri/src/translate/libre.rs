use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::json;

use super::{Translation, HTTP};
use crate::config;

#[derive(Deserialize)]
struct LibreResponse {
    #[serde(rename = "translatedText")]
    translated_text: Option<String>,
    #[serde(rename = "detectedLanguage")]
    detected_language: Option<DetectedLanguage>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct DetectedLanguage {
    language: Option<String>,
}

/// LibreTranslate：开源引擎，推荐用 Docker 自建
///   docker run -d -p 5555:5000 -e LT_LOAD_ONLY=en,zh libretranslate/libretranslate
/// 宿主端口不能用 5000：macOS 的控制中心（AirPlay 接收器）常驻占用该端口，
/// 照着官方示例映射 5000:5000 会直接撞车，且症状是连上了但返回 403。
/// 公共实例大多已关闭或需要 API Key，所以默认指向 localhost。
pub async fn translate(text: &str, source: &str, target: &str) -> Result<Translation> {
    let cfg = config::get();
    let base = cfg.libre_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(anyhow!("未配置 LibreTranslate 地址，请到设置里填写。"));
    }

    // LibreTranslate 用两字母代码，zh-CN / zh-TW 都归到 zh
    let lang = if target.starts_with("zh") {
        "zh"
    } else {
        target
    };

    // LibreTranslate 的源语言也用两字母代码，zh-CN / zh-TW 统一成 zh
    let source_lang = if source.starts_with("zh") { "zh" } else { source };

    let mut payload = json!({
        "q": text,
        "source": source_lang,
        "target": lang,
        "format": "text",
    });
    if !cfg.libre_api_key.trim().is_empty() {
        payload["api_key"] = json!(cfg.libre_api_key.trim());
    }

    let response = HTTP
        .post(format!("{base}/translate"))
        .json(&payload)
        .send()
        .await
        .map_err(|e| anyhow!("连接 LibreTranslate 失败（{base}）：{e}"))?;

    let status = response.status();
    let parsed: LibreResponse = response
        .json()
        .await
        .map_err(|e| anyhow!("LibreTranslate 返回了无法解析的内容（HTTP {status}）：{e}"))?;

    if let Some(err) = parsed.error {
        return Err(anyhow!("LibreTranslate 报错：{err}"));
    }

    let translated = parsed
        .translated_text
        .ok_or_else(|| anyhow!("LibreTranslate 返回了空结果（HTTP {status}）"))?;

    Ok(Translation {
        text: translated,
        provider: "LibreTranslate".into(),
        target: target.to_string(),
        detected_source: parsed.detected_language.and_then(|d| d.language),
    })
}

#[cfg(test)]
mod tests {
    /// 打通自建的 LibreTranslate 实例。默认不跑（需要本地容器）：
    ///   docker run -d -p 5555:5000 -e LT_LOAD_ONLY=en,zh libretranslate/libretranslate
    ///   cargo test --lib -- --ignored --nocapture libre_round_trip
    #[tokio::test]
    #[ignore]
    async fn libre_round_trip() {
        let result = super::translate("The quick brown fox jumps over the lazy dog.", "auto", "zh-CN")
            .await
            .expect("LibreTranslate 请求失败");

        println!("译文: {}", result.text);
        println!("识别源语言: {:?}", result.detected_source);

        assert!(!result.text.is_empty(), "译文为空");
        assert!(
            result.text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "译文里没有中文，语言代码归一化可能不对（该实例用 zh-Hans 而不是 zh）"
        );
    }
}
