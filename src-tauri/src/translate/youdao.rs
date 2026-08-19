//! 有道智云翻译。国内可直连，不需要代理——这正是它存在的意义。
//!
//! 签名规则（v3）：sign = SHA256(appKey + truncate(q) + salt + curtime + appSecret)
//! truncate 的定义很别扭：长度 ≤ 20 时取全文，否则取「前 10 字 + 长度 + 后 10 字」，
//! 而且按**字符**算不是字节。签错了只会返回一个笼统的错误码，很难查。

use anyhow::{anyhow, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::{Translation, HTTP};
use crate::config;

const API_URL: &str = "https://openapi.youdao.com/api";

#[derive(Deserialize)]
struct YoudaoResponse {
    #[serde(rename = "errorCode")]
    error_code: Option<String>,
    translation: Option<Vec<String>>,
    #[serde(rename = "l")]
    langs: Option<String>,
}

/// 有道的语言代码：中文用 zh-CHS，其余基本是两字母
fn to_youdao_lang(code: &str) -> String {
    match code {
        "auto" | "" => "auto".into(),
        "zh-CN" | "zh" => "zh-CHS".into(),
        "zh-TW" | "zh-HK" => "zh-CHT".into(),
        other => other.split('-').next().unwrap_or(other).to_string(),
    }
}

/// 见文件头：按字符截断，不是字节
fn truncate(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    if len <= 20 {
        return text.to_string();
    }
    let head: String = chars[..10].iter().collect();
    let tail: String = chars[len - 10..].iter().collect();
    format!("{head}{len}{tail}")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub async fn translate(text: &str, source: &str, target: &str) -> Result<Translation> {
    let cfg = config::get();
    let app_key = cfg.youdao_app_key.trim();
    let app_secret = cfg.youdao_app_secret.trim();
    if app_key.is_empty() || app_secret.is_empty() {
        return Err(anyhow!("未配置有道的应用 ID / 密钥，请到设置里填写。"));
    }

    // salt 用时间戳即可，有道只要求同一次请求里前后一致
    let curtime = now_secs().to_string();
    let salt = format!("{}", now_secs());

    let raw = format!("{app_key}{}{salt}{curtime}{app_secret}", truncate(text));
    let sign = hex::encode(Sha256::digest(raw.as_bytes()));

    let response = HTTP
        .post(API_URL)
        .form(&[
            ("q", text),
            ("from", &to_youdao_lang(source)),
            ("to", &to_youdao_lang(target)),
            ("appKey", app_key),
            ("salt", &salt),
            ("sign", &sign),
            ("signType", "v3"),
            ("curtime", &curtime),
        ])
        .send()
        .await?;

    let parsed: YoudaoResponse = response.json().await?;

    match parsed.error_code.as_deref() {
        Some("0") | None => {}
        Some(code) => {
            return Err(anyhow!(
                "有道翻译失败（错误码 {code}）。常见原因：密钥不对、余额用尽、或该语言对不支持。"
            ))
        }
    }

    let translated = parsed
        .translation
        .and_then(|v| v.into_iter().next())
        .ok_or_else(|| anyhow!("有道返回了空结果"))?;

    // l 形如 "en2zh-CHS"，取前半段作为识别到的源语言
    let detected = parsed
        .langs
        .and_then(|l| l.split('2').next().map(str::to_string));

    Ok(Translation {
        text: translated,
        provider: "有道翻译".into(),
        target: target.to_string(),
        detected_source: detected,
    })
}
