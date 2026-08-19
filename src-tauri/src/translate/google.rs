use anyhow::{anyhow, Result};
use serde_json::Value;

use super::{chunk, Translation, HTTP};

/// Google 翻译的网页端免费端点。无需 API Key，是开箱即用的默认引擎。
/// 注意这是非官方接口：高频调用可能被限流，网络受限地区可能直连不通。
async fn translate_chunk(
    text: &str,
    source: &str,
    target: &str,
) -> Result<(String, Option<String>)> {
    // sl=auto 就是让 Google 自己识别；指定了源语言就照用，
    // 短词和简繁体这类容易认错的情况下用户手动指定会更准
    let url = format!(
        "https://translate.googleapis.com/translate_a/single\
         ?client=gtx&sl={}&tl={}&dt=t&q={}",
        urlencoding::encode(source),
        urlencoding::encode(target),
        urlencoding::encode(text)
    );

    let response = HTTP.get(&url).send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!(
            "Google 翻译失败：HTTP {}。若长期失败，请到设置里换一个引擎。",
            status.as_u16()
        ));
    }

    let body: Value = response.json().await?;

    // 返回结构形如 [[["译文","原文",...], ...], null, "en", ...]
    let segments = body
        .get(0)
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Google 翻译返回了无法解析的结构"))?;

    let translated: String = segments
        .iter()
        .filter_map(|seg| seg.get(0).and_then(Value::as_str))
        .collect();

    let detected = body.get(2).and_then(Value::as_str).map(str::to_string);

    Ok((translated, detected))
}

pub async fn translate(text: &str, source: &str, target: &str) -> Result<Translation> {
    // 端点走 URL query，太长会 414，这里按 1500 字切块
    let chunks = chunk(text, 1500);
    let mut parts = Vec::with_capacity(chunks.len());
    let mut detected_source = None;

    for part in chunks {
        let (translated, detected) = translate_chunk(&part, source, target).await?;
        parts.push(translated);
        if detected_source.is_none() {
            detected_source = detected;
        }
    }

    Ok(Translation {
        text: parts.join("\n"),
        provider: "Google".into(),
        target: target.to_string(),
        detected_source,
    })
}
