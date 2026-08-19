use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::RwLock;

/// 划词后的触发方式
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TriggerMode {
    /// 划完只弹一个小图标条，点了才动作（默认，防误触）
    Bubble,
    /// 划完直接翻译并弹结果面板
    Auto,
    /// 完全不监听鼠标，只用快捷键
    Hotkey,
}

impl Default for TriggerMode {
    fn default() -> Self {
        TriggerMode::Bubble
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hotkeys {
    pub translate: String,
    /// 截图翻译：框选 → OCR → 翻译
    pub ocr: String,
    /// 截图提取：框选 → OCR → 抠出文字/号码/链接，不翻译
    #[serde(default = "default_extract_hotkey")]
    pub extract: String,
    pub collect: String,
    pub collector: String,
}

fn default_extract_hotkey() -> String {
    "Alt+Shift+E".into()
}

impl Default for Hotkeys {
    fn default() -> Self {
        // macOS 上 Alt = Option，写成 Alt+Shift 三端一致
        Self {
            translate: "Alt+Shift+T".into(),
            ocr: "Alt+Shift+A".into(),
            extract: default_extract_hotkey(),
            collect: "Alt+Shift+C".into(),
            collector: "Alt+Shift+D".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// 总开关：关掉后不再监听划词
    pub enabled: bool,
    pub trigger_mode: TriggerMode,
    /// 当前翻译引擎 id
    pub provider: String,
    /// 目标语言，"auto" = 中文→英文，其它→中文
    pub target_lang: String,
    /// 源语言，"auto" = 交给引擎自动识别。
    /// 大多数时候自动识别够用，但短词、混合语言、简繁体这些场合会认错，
    /// 所以留一个手动指定的口子。
    ///
    /// 必须显式指定 default：容器上的 `#[serde(default)]` 对缺失字段用的是
    /// **字段类型**的默认值，String 会变成空串而不是 "auto"，空串传给
    /// Google 的 sl= 参数会出问题。老版本配置文件里没有这个字段，一定会走到。
    #[serde(default = "default_auto")]
    pub source_lang: String,
    /// 鼠标拖动多少像素才算一次划选，防误触
    pub drag_threshold: f64,
    /// 双击选词是否也触发
    pub trigger_on_double_click: bool,
    pub min_length: usize,
    pub max_length: usize,
    /// 翻译前把 snake_case / kebab-case 标识符拆成空格（程序员场景）
    pub split_identifiers: bool,
    /// 开机自启
    pub autostart: bool,
    /// 界面主题："system" 跟随系统 / "light" 浅色 / "dark" 深色
    #[serde(default = "default_system")]
    pub theme: String,
    /// 是否自动检查更新。
    ///
    /// 必须显式指定 default：bool 的默认值是 false，老配置文件里没有这个字段时
    /// 会被填成 false，等于**静默关掉自动更新**——用户什么都没做却再也收不到新版本。
    /// （source_lang 当初就踩过同一个坑。）
    #[serde(default = "default_true")]
    pub auto_check_update: bool,
    /// 是否已经提示过「可以下载离线语言包」。
    /// 这个提示只在联网时出现一次——断网时提示下载毫无意义，那会儿也下不了。
    pub offline_hint_dismissed: bool,

    pub hotkeys: Hotkeys,

    /// OCR 识别语言，顺序影响识别倾向
    pub ocr_languages: Vec<String>,

    // ---- 各引擎的可选凭据，不填就不在界面上出现 ----
    pub deepl_api_key: String,
    pub deepl_pro: bool,
    pub claude_api_key: String,
    pub claude_model: String,
    pub libre_url: String,
    pub libre_api_key: String,

    // ---- OpenAI 兼容接口 ----
    // 一套实现覆盖一大批服务：OpenAI、DeepSeek、Kimi、智谱、通义、
    // OpenRouter、Groq，以及 Ollama / LM Studio 这类本地服务，
    // 区别只在 base_url 和 model 两个字段。
    pub openai_api_key: String,
    #[serde(default = "default_openai_base")]
    pub openai_base_url: String,
    #[serde(default = "default_openai_model")]
    pub openai_model: String,

    // ---- 有道智云（国内可直连）----
    pub youdao_app_key: String,
    pub youdao_app_secret: String,

    // ---- 百度翻译开放平台（国内可直连）----
    pub baidu_app_id: String,
    pub baidu_secret: String,
}

fn default_openai_base() -> String {
    "https://api.openai.com/v1".into()
}

fn default_openai_model() -> String {
    "gpt-4o-mini".into()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            trigger_mode: TriggerMode::default(),
            // 这里填的是「联网时用哪个云端引擎」。
            // 断网或云端不通时会自动回落到本地引擎（macOS 上是系统翻译），
            // 所以默认值不影响离线可用性。想强制离线就把它设成 "system"。
            provider: "google".into(),
            target_lang: "auto".into(),
            source_lang: "auto".into(),
            drag_threshold: 6.0,
            trigger_on_double_click: true,
            min_length: 1,
            max_length: 8000,
            split_identifiers: true,
            autostart: false,
            theme: default_system(),
            auto_check_update: true,
            offline_hint_dismissed: false,
            hotkeys: Hotkeys::default(),
            ocr_languages: vec!["zh-Hans".into(), "en-US".into()],
            deepl_api_key: String::new(),
            deepl_pro: false,
            claude_api_key: String::new(),
            claude_model: "claude-opus-5".into(),
            libre_url: "http://localhost:5555".into(),
            libre_api_key: String::new(),
            openai_api_key: String::new(),
            openai_base_url: default_openai_base(),
            openai_model: default_openai_model(),
            youdao_app_key: String::new(),
            youdao_app_secret: String::new(),
            baidu_app_id: String::new(),
            baidu_secret: String::new(),
        }
    }
}

fn default_auto() -> String {
    "auto".into()
}

fn default_true() -> bool {
    true
}

fn default_system() -> String {
    "system".into()
}

static CONFIG: Lazy<RwLock<Config>> = Lazy::new(|| RwLock::new(load_from_disk()));

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("SnapLingo")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

fn load_from_disk() -> Config {
    match std::fs::read_to_string(config_path()) {
        Ok(raw) => serde_json::from_str(&raw).unwrap_or_else(|err| {
            log::warn!("配置解析失败，回退到默认值: {err}");
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}

pub fn get() -> Config {
    CONFIG.read().expect("config lock poisoned").clone()
}

/// 原子写文件：先写同目录的临时文件，fsync 落盘，再 rename 覆盖。
///
/// 直接 write 覆盖的问题是中途崩溃/磁盘满会留下**截断的 JSON**——配置还能回落
/// 默认值，收集夹里的内容就真没了。rename 在同一文件系统内是原子的，
/// 要么是完整的旧文件，要么是完整的新文件，不存在中间状态。
pub fn write_atomic(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    use std::io::Write;

    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;

    // 临时文件必须和目标在同一目录：跨文件系统的 rename 不是原子的，
    // 放 /tmp 再 rename 到用户目录就失去了这个保证。
    let temp = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(contents.as_bytes())?;
        file.flush()?;
        // 光 flush 只把数据交给系统缓冲，断电仍可能丢；sync_all 才落到盘上
        file.sync_all()?;
    }
    std::fs::rename(&temp, path)?;
    Ok(())
}

pub fn save(next: Config) -> anyhow::Result<Config> {
    // 先落盘再更新内存：写失败时内存状态不该已经变了，
    // 否则界面显示的是新值、重启后又变回旧值，对不上。
    write_atomic(&config_path(), &serde_json::to_string_pretty(&next)?)?;
    {
        let mut guard = CONFIG.write().expect("config lock poisoned");
        *guard = next.clone();
    }
    Ok(next)
}

/// 只改一部分字段
pub fn update<F: FnOnce(&mut Config)>(mutate: F) -> anyhow::Result<Config> {
    let mut next = get();
    mutate(&mut next);
    save(next)
}

/// 判断文本主体是不是 CJK，用于解析 "auto" 目标语言
pub fn is_mostly_cjk(text: &str) -> bool {
    let mut cjk = 0usize;
    let mut latin = 0usize;
    for ch in text.chars() {
        let c = ch as u32;
        let is_cjk = (0x3040..=0x30FF).contains(&c)      // 日文假名
            || (0x3400..=0x4DBF).contains(&c)            // CJK 扩展 A
            || (0x4E00..=0x9FFF).contains(&c)            // CJK 基本区
            || (0xF900..=0xFAFF).contains(&c)            // 兼容汉字
            || (0xAC00..=0xD7AF).contains(&c); // 韩文
        if is_cjk {
            cjk += 1;
        } else if ch.is_ascii_alphabetic() {
            latin += 1;
        }
    }
    cjk > 0 && cjk >= latin
}

/// 把 "auto" 解析成具体目标语言
pub fn resolve_target(text: &str, configured: &str) -> String {
    if configured != "auto" {
        return configured.to_string();
    }
    if is_mostly_cjk(text) {
        "en".to_string()
    } else {
        "zh-CN".to_string()
    }
}

/// 支持的源语言列表，给前端下拉框用。
///
/// 和目标语言是同一批语言，只有 "auto" 的含义不同：这里是「让引擎自己识别」，
/// 目标语言那边是「中英互转」。所以标签必须分开写，不能复用同一个列表。
pub fn source_languages() -> Vec<(&'static str, &'static str)> {
    let mut list = vec![("auto", "自动检测")];
    list.extend(target_languages().into_iter().filter(|(code, _)| *code != "auto"));
    list
}

/// 支持的目标语言列表，给前端下拉框用
pub fn target_languages() -> Vec<(&'static str, &'static str)> {
    vec![
        ("auto", "自动（中↔英）"),
        ("zh-CN", "简体中文"),
        ("zh-TW", "繁體中文"),
        ("en", "English"),
        ("ja", "日本語"),
        ("ko", "한국어"),
        ("fr", "Français"),
        ("de", "Deutsch"),
        ("es", "Español"),
        ("ru", "Русский"),
        ("pt", "Português"),
        ("it", "Italiano"),
    ]
}
