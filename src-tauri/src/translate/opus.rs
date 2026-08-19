//! OPUS-MT 离线翻译引擎（CTranslate2 int8 推理）。
//!
//! 定位是**跨平台的离线备选**：macOS 15+ 上系统翻译更好也更省空间，这里主要
//! 服务于老 macOS、Windows / Linux，以及系统翻译不支持的语言对。
//!
//! 模型不进安装包，由 localmodel.rs 按需下载。这里只负责推理。
//!
//! 实测数据（M 系列，中英方向）：模型加载约 50ms，一句话推理约 70ms。
//! 因为加载够快，这里**不缓存 translator**——用完即释放，空闲时不占内存，
//! 符合「常驻后台但不吃资源」的定位。

use anyhow::{anyhow, Result};
use ct2rs::tokenizers::sentencepiece::Tokenizer;

use super::Translation;
use crate::localmodel;

/// OPUS-MT 是单向模型，必须知道确切的源语言。
///
/// 源语言是 auto 时只能猜：按 CJK 占比判断中文还是英文。这覆盖了绝大多数
/// 实际用法（中英互译），其它语言在自动检测下会落到 en。
fn resolve_direction(text: &str, source: &str, target: &str) -> (String, String) {
    let base = |code: &str| code.split('-').next().unwrap_or(code).to_lowercase();

    let from = if source == "auto" || source.is_empty() {
        if crate::config::is_mostly_cjk(text) {
            "zh".to_string()
        } else {
            "en".to_string()
        }
    } else {
        base(source)
    };

    (from, base(target))
}

/// 清理 sentencepiece 解码残留。
///
/// ct2rs 的解码偶尔会把词边界符 U+2581（▁）原样吐出来，中文标点前也会多空格。
/// 这些是解码层的毛病，不清掉直接就显示给用户了。
fn clean(text: &str) -> String {
    let stripped: String = text.replace('\u{2581}', " ");
    // 复用 OCR 那套 CJK 空格清理：中文字符之间的空格要吃掉，中英之间要留
    crate::ocr::normalize(stripped.trim())
}

/// 按句切分。
///
/// OPUS-MT 有输入长度上限，长文必须切开；按句子切而不是按字数硬切，
/// 否则会在句子中间断开，译文质量会明显崩坏。
fn split_sentences(text: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();

    for line in text.split('\n') {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.chars().count() <= limit {
            out.push(line.to_string());
            continue;
        }

        let mut current = String::new();
        for ch in line.chars() {
            current.push(ch);
            let ends_sentence = matches!(ch, '.' | '!' | '?' | '。' | '！' | '？' | '；' | ';');
            if ends_sentence && current.chars().count() >= limit / 2 {
                out.push(std::mem::take(&mut current).trim().to_string());
            } else if current.chars().count() >= limit {
                // 一句话本身就超长（少见），只能硬切
                out.push(std::mem::take(&mut current).trim().to_string());
            }
        }
        if !current.trim().is_empty() {
            out.push(current.trim().to_string());
        }
    }

    if out.is_empty() {
        out.push(text.trim().to_string());
    }
    out
}

fn translate_blocking(text: &str, source: &str, target: &str) -> Result<Translation> {
    let (from, to) = resolve_direction(text, source, target);

    let model_id = localmodel::installed_for(&from, &to).ok_or_else(|| {
        anyhow!(
            "{MODEL_MISSING}:{from}-{to}"
        )
    })?;

    let dir = localmodel::model_dir(&model_id);
    // Argos 包用单个 sentencepiece.model（共享词表），
    // 所以编码器和解码器指向同一个文件
    let spm = dir.join("sentencepiece.model");
    let tokenizer = Tokenizer::from_file(&spm, &spm)
        .map_err(|e| anyhow!("加载分词模型失败: {e}"))?;

    let translator = ct2rs::Translator::with_tokenizer(
        dir.join("model"),
        tokenizer,
        &ct2rs::Config::default(),
    )
    .map_err(|e| anyhow!("加载离线模型失败: {e}"))?;

    let segments = split_sentences(text, 180);
    let output = translator
        .translate_batch(&segments, &ct2rs::TranslationOptions::default(), None)
        .map_err(|e| anyhow!("离线翻译失败: {e}"))?;

    let joined = output
        .into_iter()
        .map(|(text, _score)| clean(&text))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("");

    Ok(Translation {
        text: joined,
        provider: "离线模型".into(),
        target: target.to_string(),
        detected_source: Some(from),
    })
}

/// 前端靠这个前缀识别「该语言方向的离线模型还没下载」
pub const MODEL_MISSING: &str = "OPUS_MODEL_MISSING";

/// 推理是纯 CPU 的阻塞活，挪到线程池，别占住异步运行时
pub async fn translate(text: String, source: String, target: String) -> Result<Translation> {
    tokio::task::spawn_blocking(move || translate_blocking(&text, &source, &target))
        .await
        .map_err(|e| anyhow!("离线翻译任务异常: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sentencepiece_marker() {
        // 解码残留的 ▁ 必须清掉，否则直接显示给用户
        assert_eq!(clean("▁在前一章中,我们看到"), "在前一章中,我们看到");
        // 中文标点前的多余空格也要清掉
        assert_eq!(clean("配置连接超时 。"), "配置连接超时。");
    }

    #[test]
    fn guesses_direction_from_text() {
        assert_eq!(resolve_direction("Hello world", "auto", "zh-CN"), ("en".into(), "zh".into()));
        assert_eq!(resolve_direction("你好世界", "auto", "en"), ("zh".into(), "en".into()));
        // 明确指定时不猜
        assert_eq!(resolve_direction("Hello", "fr", "zh-CN"), ("fr".into(), "zh".into()));
    }

    #[test]
    fn splits_on_sentence_boundaries() {
        let long = "First sentence here. Second sentence follows. Third one ends it.";
        let parts = split_sentences(long, 24);
        assert!(parts.len() > 1, "长文本应该被切开");
        // 不能吞字
        let rejoined: String = parts.join(" ");
        assert!(rejoined.contains("Third one"));
    }

    /// 端到端跑一遍真实推理：模型查找 → 分词 → CTranslate2 → 清理。
    /// 需要已经下载好 en-zh 模型，所以默认不跑：
    ///   cargo test --lib -- --ignored --nocapture real_offline
    #[test]
    #[ignore]
    fn real_offline_translation() {
        let Some(id) = localmodel::installed_for("en", "zh") else {
            panic!("没有已安装的 en→zh 模型，先在设置里下载");
        };
        println!("使用模型: {id}");

        let started = std::time::Instant::now();
        let result = translate_blocking(
            "The quick brown fox jumps over the lazy dog.",
            "auto",
            "zh-CN",
        )
        .expect("离线翻译失败");
        println!("耗时: {:?}", started.elapsed());
        println!("译文: {}", result.text);

        assert!(!result.text.is_empty(), "译文为空");
        assert!(
            result.text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
            "译文里没有中文字符，翻译没真正发生"
        );
        // 解码残留必须已经清掉
        assert!(!result.text.contains('\u{2581}'), "译文里还有 ▁ 残留");
        assert_eq!(result.provider, "离线模型");
    }

    #[test]
    fn keeps_short_text_intact() {
        assert_eq!(split_sentences("Hello world.", 180), vec!["Hello world."]);
    }
}
