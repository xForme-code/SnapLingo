//! 百度翻译开放平台。国内可直连，申请门槛是几家里最低的。
//!
//! 签名规则：sign = MD5(appid + q + salt + 密钥)，全小写十六进制。

use anyhow::{anyhow, Result};
use md5::{Digest, Md5};
use serde::Deserialize;

use super::{Translation, HTTP};
use crate::config;

const API_URL: &str = "https://fanyi-api.baidu.com/api/trans/vip/translate";

#[derive(Deserialize)]
struct BaiduResponse {
    error_code: Option<String>,
    error_msg: Option<String>,
    from: Option<String>,
    trans_result: Option<Vec<TransItem>>,
}

#[derive(Deserialize)]
struct TransItem {
    dst: String,
}

/// 百度的语言代码：中文是 zh，繁体是 cht，日语是 jp（不是 ja）
fn to_baidu_lang(code: &str) -> String {
    match code {
        "auto" | "" => "auto".into(),
        "zh-CN" | "zh" => "zh".into(),
        "zh-TW" | "zh-HK" => "cht".into(),
        "ja" => "jp".into(),
        "ko" => "kor".into(),
        "fr" => "fra".into(),
        "es" => "spa".into(),
        "ar" => "ara".into(),
        other => other.split('-').next().unwrap_or(other).to_string(),
    }
}

pub async fn translate(text: &str, source: &str, target: &str) -> Result<Translation> {
    let cfg = config::get();
    let app_id = cfg.baidu_app_id.trim();
    let secret = cfg.baidu_secret.trim();
    if app_id.is_empty() || secret.is_empty() {
        return Err(anyhow!("未配置百度的 APP ID / 密钥，请到设置里填写。"));
    }

    let salt = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "1435660288".into());

    let sign = hex::encode(Md5::digest(
        format!("{app_id}{text}{salt}{secret}").as_bytes(),
    ));

    let response = HTTP
        .post(API_URL)
        .form(&[
            ("q", text),
            ("from", &to_baidu_lang(source)),
            ("to", &to_baidu_lang(target)),
            ("appid", app_id),
            ("salt", &salt),
            ("sign", &sign),
        ])
        .send()
        .await?;

    let parsed: BaiduResponse = response.json().await?;

    if let Some(code) = parsed.error_code.as_deref() {
        if code != "52000" {
            let msg = parsed.error_msg.unwrap_or_else(|| "未知错误".into());
            return Err(anyhow!("百度翻译失败（{code}）：{msg}"));
        }
    }

    // 百度按行返回，逐段拼回去
    let translated = parsed
        .trans_result
        .map(|items| {
            items
                .into_iter()
                .map(|i| i.dst)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("百度返回了空结果"))?;

    Ok(Translation {
        text: translated,
        provider: "百度翻译".into(),
        target: target.to_string(),
        detected_source: parsed.from,
    })
}
