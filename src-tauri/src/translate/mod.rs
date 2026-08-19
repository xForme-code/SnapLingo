pub mod baidu;
pub mod claude;
pub mod deepl;
pub mod google;
pub mod libre;
pub mod openai;
pub mod opus;
pub mod system;
pub mod youdao;

use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use serde::Serialize;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config;

/// 云端引擎的时间预算。
///
/// 断网时连接会一直悬着直到 TCP 超时，划词场景等不起。给一个短预算，
/// 超了就当云端不可用、立刻回落本地——用户宁可拿到本地译文，
/// 也不想对着转圈等十几秒。
const CLOUD_BUDGET: Duration = Duration::from_secs(4);

/// 云端失败后的冷却期。
///
/// 断网时每次划词都去试一遍云端，等于每次都白等 4 秒。记住上次失败，
/// 冷却期内直接走本地，用户体验才是「离线也顺手」。
const CLOUD_COOLDOWN: Duration = Duration::from_secs(60);

static CLOUD_DOWN_UNTIL: Lazy<Mutex<Option<Instant>>> = Lazy::new(|| Mutex::new(None));

fn cloud_in_cooldown() -> bool {
    CLOUD_DOWN_UNTIL
        .lock()
        .ok()
        .and_then(|slot| *slot)
        .is_some_and(|until| Instant::now() < until)
}

fn note_cloud_down() {
    if let Ok(mut slot) = CLOUD_DOWN_UNTIL.lock() {
        *slot = Some(Instant::now() + CLOUD_COOLDOWN);
    }
}

fn note_cloud_ok() {
    if let Ok(mut slot) = CLOUD_DOWN_UNTIL.lock() {
        *slot = None;
    }
}

/// 全局共用一个 HTTP client，复用连接、统一超时
pub static HTTP: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .connect_timeout(Duration::from_secs(8))
        .user_agent(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
        )
        .build()
        .expect("构建 HTTP client 失败")
});

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Translation {
    pub text: String,
    /// 实际使用的引擎显示名
    pub provider: String,
    /// 实际使用的目标语言
    pub target: String,
    /// 引擎识别出的源语言，可能为空
    pub detected_source: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub id: &'static str,
    pub label: &'static str,
    /// 是否需要填 API Key 才能用
    pub needs_key: bool,
    /// 当前配置下是否可用（缺 Key 的引擎前端会置灰）
    pub available: bool,
    pub note: &'static str,
}

pub fn list_providers() -> Vec<ProviderInfo> {
    let cfg = config::get();
    vec![
        ProviderInfo {
            id: "system",
            label: "系统翻译（离线）",
            needs_key: false,
            // macOS 15+ 才有；低版本上 sidecar 编译时就被跳过了，
            // 真正调用时会失败并回落到其它引擎
            available: cfg!(target_os = "macos"),
            note: "系统内置，完全离线、免费、不受网络环境影响。首次使用需下载语言包。",
        },
        ProviderInfo {
            id: "opus",
            label: "离线模型（OPUS-MT）",
            needs_key: false,
            available: true,
            note: "完全离线，跨平台可用。需要先在下面按语言方向下载模型。",
        },
        ProviderInfo {
            id: "google",
            label: "Google 翻译",
            needs_key: false,
            available: true,
            note: "免费、无需配置。非官方接口，高频使用可能限流。",
        },
        ProviderInfo {
            id: "libre",
            label: "LibreTranslate",
            needs_key: false,
            available: !cfg.libre_url.trim().is_empty(),
            note: "开源引擎，建议用 Docker 自建，数据不出内网。",
        },
        ProviderInfo {
            id: "youdao",
            label: "有道翻译（国内直连）",
            needs_key: true,
            available: !cfg.youdao_app_key.trim().is_empty()
                && !cfg.youdao_app_secret.trim().is_empty(),
            note: "国内可直连、不需要代理，中英质量好。需在有道智云申请，有免费额度。",
        },
        ProviderInfo {
            id: "baidu",
            label: "百度翻译（国内直连）",
            needs_key: true,
            available: !cfg.baidu_app_id.trim().is_empty() && !cfg.baidu_secret.trim().is_empty(),
            note: "国内可直连、不需要代理。申请门槛最低，标准版有免费额度。",
        },
        ProviderInfo {
            id: "openai",
            label: "OpenAI 兼容接口",
            needs_key: true,
            available: !cfg.openai_api_key.trim().is_empty()
                || cfg.openai_base_url.contains("localhost")
                || cfg.openai_base_url.contains("127.0.0.1"),
            note: "填不同的接口地址即可接入 OpenAI / DeepSeek / Kimi / 智谱 / 通义 / OpenRouter，以及 Ollama、LM Studio 等本地服务。",
        },
        ProviderInfo {
            id: "deepl",
            label: "DeepL",
            needs_key: true,
            available: !cfg.deepl_api_key.trim().is_empty(),
            note: "免费版每月 50 万字符，需要注册 API Key。",
        },
        ProviderInfo {
            id: "claude",
            label: "Claude",
            needs_key: true,
            available: !cfg.claude_api_key.trim().is_empty(),
            note: "长句和专业术语质量最好，按量付费。",
        },
    ]
}

/// 翻译入口：解析目标语言 → 预处理 → 分发到具体引擎
///
/// 需要 AppHandle 是因为系统翻译走 sidecar 子进程，要靠它定位可执行文件。
pub async fn translate(
    app: &tauri::AppHandle,
    text: &str,
    source: Option<&str>,
    target: Option<&str>,
    provider: Option<&str>,
) -> Result<Translation> {
    let cfg = config::get();

    let raw = text.trim();
    if raw.is_empty() {
        return Err(anyhow!("没有可翻译的内容"));
    }

    let prepared = if cfg.split_identifiers {
        crate::selection::split_identifiers(raw)
    } else {
        raw.to_string()
    };

    let target_lang = config::resolve_target(&prepared, target.unwrap_or(&cfg.target_lang));
    // 空串一律当自动检测：前端可能传空，老配置文件也可能没这个字段
    let source_lang = match source.unwrap_or(&cfg.source_lang).trim() {
        "" => "auto".to_string(),
        value => value.to_string(),
    };
    let provider_id = provider.unwrap_or(&cfg.provider).to_string();

    // 用户明确选了某个离线引擎：那就只用本地，没有回落云端的必要
    if provider_id == "system" || provider_id == "opus" {
        return dispatch(app, &provider_id, &prepared, &source_lang, &target_lang).await;
    }

    // 否则走「联网优先、断网回落」：
    // 云端质量更好，能用就用；连不上就自动切本地，用户不需要知道发生了什么。
    if cloud_in_cooldown() {
        log::debug!("云端处于冷却期，直接走本地翻译");
    } else {
        match tokio::time::timeout(
            CLOUD_BUDGET,
            dispatch(app, &provider_id, &prepared, &source_lang, &target_lang),
        )
        .await
        {
            Ok(Ok(translation)) => {
                note_cloud_ok();
                return Ok(translation);
            }
            // 引擎自己报错（限流、Key 失效、返回结构变了）同样说明这条路当下不通
            Ok(Err(err)) => {
                log::warn!("云端引擎 {provider_id} 失败，回落本地: {err}");
                note_cloud_down();
            }
            Err(_) => {
                log::warn!("云端引擎 {provider_id} 超过 {CLOUD_BUDGET:?} 未返回，回落本地");
                note_cloud_down();
            }
        }
    }

    local_fallback(app, &prepared, &source_lang, &target_lang).await
}

/// 本地两级回落：系统翻译 → OPUS-MT 离线模型。
///
/// 系统翻译优先（质量更好、零磁盘占用），它不可用时才轮到 OPUS-MT——
/// 那是 macOS 15 以下、Windows / Linux，以及系统翻译不支持的语言对的兜底。
async fn local_fallback(
    app: &tauri::AppHandle,
    prepared: &str,
    source_lang: &str,
    target_lang: &str,
) -> Result<Translation> {
    // 先问一句系统语言包装了没，没装就**不要**去调它。
    //
    // 直接调用的后果：系统会弹出自己的「下载语言以翻译」对话框。用户明明已经
    // 下好了我们的 OPUS-MT 离线模型，却被一个无关的系统弹框打断，还得手动关掉。
    // 查询走缓存（5 分钟），不会给每次翻译都添一次子进程开销。
    let pack = {
        let handle = app.clone();
        let (src, tgt) = (source_lang.to_string(), target_lang.to_string());
        tokio::task::spawn_blocking(move || system::availability_cached(&handle, &src, &tgt))
            .await
            .unwrap_or(system::Availability::Unavailable)
    };

    let system_err = if pack == system::Availability::Installed {
        match dispatch(app, "system", prepared, source_lang, target_lang).await {
            Ok(translation) => return Ok(translation),
            Err(err) => err,
        }
    } else {
        log::debug!("系统翻译语言包未就绪（{pack:?}），跳过它直接用离线模型");
        anyhow!("{}", system::NEEDS_DOWNLOAD)
    };

    // OPUS-MT 模型已经下载好的话，它比「让用户去下系统语言包」更直接可用
    match dispatch(app, "opus", prepared, source_lang, target_lang).await {
        Ok(translation) => return Ok(translation),
        Err(opus_err) => {
            log::debug!("离线模型也不可用: {opus_err}");
        }
    }

    // 两条本地路都不成。优先透出「系统语言包没下载」——那是用户点一下就能解决的，
    // 前端要靠这个标记显示下载引导。
    if system_err.to_string().contains(system::NEEDS_DOWNLOAD) {
        return Err(system_err);
    }
    Err(anyhow!(
        "云端和本地都不可用。云端：网络似乎不通；本地：{system_err}"
    ))
}

async fn dispatch(
    app: &tauri::AppHandle,
    provider_id: &str,
    text: &str,
    source_lang: &str,
    target_lang: &str,
) -> Result<Translation> {
    match provider_id {
        "system" => {
            system::translate(
                app.clone(),
                text.to_string(),
                source_lang.to_string(),
                target_lang.to_string(),
            )
            .await
        }
        "opus" => {
            opus::translate(text.to_string(), source_lang.to_string(), target_lang.to_string()).await
        }
        "google" => google::translate(text, source_lang, target_lang).await,
        "libre" => libre::translate(text, source_lang, target_lang).await,
        "deepl" => deepl::translate(text, source_lang, target_lang).await,
        "claude" => claude::translate(text, source_lang, target_lang).await,
        "openai" => openai::translate(text, source_lang, target_lang).await,
        "youdao" => youdao::translate(text, source_lang, target_lang).await,
        "baidu" => baidu::translate(text, source_lang, target_lang).await,
        other => Err(anyhow!("未知的翻译引擎: {other}")),
    }
}

/// 把长文本按行切成不超过 limit 的块，尽量不在句子中间断开
pub fn chunk(text: &str, limit: usize) -> Vec<String> {
    if text.chars().count() <= limit {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();

    for line in text.split('\n') {
        if current.chars().count() + line.chars().count() + 1 > limit && !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
        }
        if line.chars().count() > limit {
            // 单行超长，只能硬切
            let mut buf = String::new();
            for ch in line.chars() {
                buf.push(ch);
                if buf.chars().count() >= limit {
                    chunks.push(std::mem::take(&mut buf));
                }
            }
            if !buf.is_empty() {
                current = buf;
            }
        } else {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(line);
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_respect_limit() {
        let text = "一二三四五\n六七八九十\n甲乙丙丁戊";
        let chunks = chunk(text, 11);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|c| c.chars().count() <= 11));
        // 切分不能吞字
        let rejoined: String = chunks.join("").chars().filter(|c| *c != '\n').collect();
        let original: String = text.chars().filter(|c| *c != '\n').collect();
        assert_eq!(rejoined, original);
    }

    /// 真实网络请求，默认不跑。手动验证：cargo test -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn google_translate_round_trip() {
        let result = google::translate("The quick brown fox jumps over the lazy dog.", "auto", "zh-CN")
            .await
            .expect("Google 翻译请求失败");

        println!("译文: {}", result.text);
        println!("识别源语言: {:?}", result.detected_source);

        assert!(!result.text.is_empty());
        assert_eq!(result.detected_source.as_deref(), Some("en"));
        // 结果里应该出现中文字符
        assert!(result.text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)));
    }
}

/// 目标语言代码 → 英文语言名，给 LLM 引擎写提示词用
pub fn language_name(code: &str) -> &'static str {
    match code {
        "zh-CN" => "Simplified Chinese",
        "zh-TW" => "Traditional Chinese",
        "en" => "English",
        "ja" => "Japanese",
        "ko" => "Korean",
        "fr" => "French",
        "de" => "German",
        "es" => "Spanish",
        "ru" => "Russian",
        "pt" => "Portuguese",
        "it" => "Italian",
        _ => "English",
    }
}
