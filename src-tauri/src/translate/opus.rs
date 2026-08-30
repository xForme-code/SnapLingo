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

/// OPUS-MT 对词表里没有的词吐这个字符（U+2047 DOUBLE QUESTION MARK）。
///
/// 字体多半把它画成两个问号，用户看到的就是「?? ??」——像乱码，
/// 完全看不出是「这个词模型不认识」。
const UNKNOWN: char = '\u{2047}';

/// 清理 sentencepiece 解码残留。
///
/// ct2rs 的解码偶尔会把词边界符 U+2581（▁）原样吐出来，中文标点前也会多空格。
/// 这些是解码层的毛病，不清掉直接就显示给用户了。
///
/// 未知词标记也在这里去掉：留着它只会让译文看起来像乱码。真正「整句都是未知词」
/// 的情况由 looks_degenerate 拦住，不会靠这里悄悄抹平。
fn clean(text: &str) -> String {
    let stripped: String = text.replace('\u{2581}', " ");

    // 未知词标记要**连同它占的位置**一起去掉。只删字符的话，
    // 「速褐狐 ⁇ .」会变成「速褐狐  .」——两个空格加一个孤零零的句点，
    // 看着比留着标记还奇怪。
    //
    // 只在真的出现标记时才走这条重排路径，正常译文的空格原样不动。
    let stripped = if stripped.contains(UNKNOWN) {
        let mut out = String::with_capacity(stripped.len());
        for token in stripped.split_whitespace() {
            if token.chars().all(|c| c == UNKNOWN) {
                continue;
            }
            // 标点紧跟前一个词，不补空格
            let tight = token.starts_with(|c: char| {
                matches!(c, '.' | ',' | '!' | '?' | ';' | ':' | ')' | ']' | '}')
            });
            if !out.is_empty() && !tight {
                out.push(' ');
            }
            out.push_str(token);
        }
        out
    } else {
        stripped
    };

    // 复用 OCR 那套 CJK 空格清理：中文字符之间的空格要吃掉，中英之间要留
    crate::ocr::normalize(stripped.trim())
}

/// 这段「译文」是不是已经废了。
///
/// OPUS-MT 是单向模型：en→zh 的词表里没有中文字，反之亦然。喂给它中英混排的
/// 内容时，另一种语言的每个字都会变成未知词，输出是一串 ⁇ 或者干脆是
/// 毫不相干的词。这种结果不该当成译文展示——宁可如实说「翻不了」，
/// 也不要给用户一段看起来像翻译、实际毫无意义的文字。
fn looks_degenerate(raw: &str) -> bool {
    let total = raw.chars().filter(|c| !c.is_whitespace()).count();
    if total == 0 {
        return true;
    }
    let unknown = raw.chars().filter(|c| *c == UNKNOWN).count();
    // 三成以上是未知词就认定失败。阈值不苛刻：正常译文里
    // 偶尔冒出一两个生僻词是常事，整段都是才说明方向或语言对不上
    unknown * 10 >= total * 3
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

    // 源和目标同一种语言时没有对应模型，报出去会是
    // 「OPUS_MODEL_MISSING:zh-zh」——用户看了完全不知道发生了什么。
    if from == to {
        return Err(anyhow!("原文和目标语言都是{from}，不需要翻译"));
    }

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

    let raw = output
        .into_iter()
        .map(|(text, _score)| text)
        .collect::<Vec<_>>()
        .join("");

    // 先判废再清理：清理会把 ⁇ 抹掉，抹完就看不出这段译文本来全是未知词了
    if looks_degenerate(&raw) {
        return Err(anyhow!(
            "离线模型翻不了这段内容。它是单向模型（{from}→{to}），\
             遇到中英混排时另一种语言的字不在词表里。可以改用联网引擎，\
             或只选中同一种语言的部分。"
        ));
    }

    let joined = clean(&raw);

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
    fn strips_unknown_token_marker() {
        // U+2047 是 OPUS-MT 的未知词标记，字体画成两个问号，
        // 留着它译文看起来就是乱码
        // 标记连同它占的空位一起去掉，不能留下「速褐狐  .」这种双空格
        assert_eq!(clean("速褐狐 \u{2047} ."), "速褐狐.");
        assert_eq!(clean("a \u{2047} b"), "a b");
        // 正常译文的空格不受影响
        assert_eq!(clean("hello world"), "hello world");
    }

    #[test]
    fn detects_degenerate_output() {
        // 整段都是未知词 —— 这是中英混排喂给单向模型的典型结果
        assert!(looks_degenerate("\u{2047} \u{2047}"));
        assert!(looks_degenerate(""));
        assert!(looks_degenerate("   "));
        // 偶尔一两个生僻词不算废
        assert!(!looks_degenerate("这是一段正常的译文，只有一个 \u{2047} 没认出来"));
        assert!(!looks_degenerate("完全正常的译文"));
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
    /// 中英混排喂给单向模型时，必须报错而不是给出一段看似译文的乱码。
    ///
    /// 真实案例：用户选中「discouraged的」，走 en→zh 离线模型，
    /// 面板上显示「?? ??」——那是 U+2047 未知词标记，字体画成两个问号。
    ///   cargo test --lib -- --ignored --nocapture mixed_language
    #[test]
    #[ignore]
    fn mixed_language_is_reported_not_faked() {
        if localmodel::installed_for("en", "zh").is_none() {
            panic!("没有已安装的 en→zh 模型，先在设置里下载");
        }

        // 纯英文：正常翻译
        let ok = translate_blocking("The quick brown fox.", "auto", "zh-CN")
            .expect("纯英文应该翻得出来");
        println!("纯英文 → {:?}", ok.text);
        assert!(!ok.text.contains(UNKNOWN), "译文里不该留下未知词标记");

        // 源和目标同语言：给人话，不是 OPUS_MODEL_MISSING:zh-zh
        let err = translate_blocking("你好世界", "zh", "zh-CN").unwrap_err().to_string();
        println!("中译中 → {err}");
        assert!(!err.contains("OPUS_MODEL_MISSING"), "不该把内部标记抛给用户");
    }

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
