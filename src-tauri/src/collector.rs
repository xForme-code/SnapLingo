use anyhow::Result;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config::config_dir;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub translation: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub target: Option<String>,
    /// "selection" | "ocr"
    pub source: String,
    pub created_at: u64,
}

static ITEMS: Lazy<RwLock<Vec<Item>>> = Lazy::new(|| RwLock::new(load()));

fn store_path() -> PathBuf {
    config_dir().join("collector.json")
}

fn load() -> Vec<Item> {
    std::fs::read_to_string(store_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn persist(items: &[Item]) {
    if let Err(err) = (|| -> Result<()> {
        // 收集夹里是用户攒下的内容，写坏了就真丢了，必须原子写
        crate::config::write_atomic(&store_path(), &serde_json::to_string_pretty(items)?)?;
        Ok(())
    })() {
        log::warn!("收集夹保存失败: {err}");
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn list() -> Vec<Item> {
    ITEMS.read().expect("collector lock poisoned").clone()
}

pub fn count() -> usize {
    ITEMS.read().expect("collector lock poisoned").len()
}

/// 加入一条。同一段原文重复收集时更新原条目而不是新增。
pub fn add(text: String, translation: Option<String>, target: Option<String>, source: String) -> Item {
    let mut items = ITEMS.write().expect("collector lock poisoned");

    if let Some(existing) = items.iter_mut().find(|i| i.text == text) {
        if translation.is_some() {
            existing.translation = translation;
            existing.target = target;
        }
        let cloned = existing.clone();
        persist(&items);
        return cloned;
    }

    let created_at = now_millis();
    let item = Item {
        id: format!("{created_at}-{}", items.len()),
        text,
        translation,
        target,
        source,
        created_at,
    };
    items.push(item.clone());
    persist(&items);
    item
}

pub fn set_translation(id: &str, translation: String, target: String) {
    let mut items = ITEMS.write().expect("collector lock poisoned");
    if let Some(item) = items.iter_mut().find(|i| i.id == id) {
        item.translation = Some(translation);
        item.target = Some(target);
        persist(&items);
    }
}

pub fn remove(id: &str) {
    let mut items = ITEMS.write().expect("collector lock poisoned");
    items.retain(|i| i.id != id);
    persist(&items);
}

pub fn clear() {
    let mut items = ITEMS.write().expect("collector lock poisoned");
    items.clear();
    persist(&items);
}

/// 合并所有原文，供「合并复制」用
pub fn merged(separator: &str) -> String {
    list()
        .iter()
        .map(|i| i.text.as_str())
        .collect::<Vec<_>>()
        .join(separator)
}

/// 原文 + 译文对照
pub fn merged_bilingual() -> String {
    list()
        .iter()
        .map(|i| match &i.translation {
            Some(t) => format!("{}\n{}", i.text, t),
            None => i.text.clone(),
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// 单条的 Markdown。用于「一键导出成文件」。
///
/// 和整体导出的区别：这里带上时间和来源做成 front matter 风格的抬头，
/// 因为单条文件往往会被丢进笔记库，脱离了收集夹的上下文就需要自带出处。
pub fn item_markdown(id: &str) -> Option<(String, String)> {
    let items = list();
    let item = items.iter().find(|i| i.id == id)?;

    let kind = if item.source == "ocr" { "截图翻译" } else { "划词" };
    let stamp = format_time(item.created_at);

    let mut out = format!("# {kind}摘录\n\n> 收集于 {stamp}\n\n{}\n", item.text);
    if let Some(translation) = &item.translation {
        let lang = item.target.as_deref().unwrap_or("译文");
        out.push_str(&format!("\n## {lang}\n\n{translation}\n"));
    }

    // 文件名取正文开头几个字，方便在文件夹里一眼认出是哪条
    let slug: String = item
        .text
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .take(24)
        .collect();
    let slug = slug.trim().replace(' ', "-");
    // 必须带上唯一后缀：文件名只取正文前 24 字，两条开头相同的内容
    // （比如从同一段话里截取的）会算出同名文件，直接写就是静默覆盖。
    let suffix = &item.id;
    let name = if slug.is_empty() {
        format!("SnapLingo-{suffix}.md")
    } else {
        format!("SnapLingo-{slug}-{suffix}.md")
    };

    Some((name, out))
}

/// 毫秒时间戳 → 本地时间字符串。不引 chrono，够用就好。
fn format_time(millis: u64) -> String {
    let secs = (millis / 1000) as i64;
    let out = std::process::Command::new("date")
        .args(["-r", &secs.to_string(), "+%Y-%m-%d %H:%M"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok());
    out.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{millis}"))
}

pub fn to_markdown() -> String {
    let items = list();
    let body = items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let kind = if item.source == "ocr" { "截图翻译" } else { "划词" };
            let quoted = item
                .translation
                .as_ref()
                .map(|t| format!("\n\n> {}", t.replace('\n', "\n> ")))
                .unwrap_or_default();
            format!("### {}. {}\n\n{}{}", index + 1, kind, item.text, quoted)
        })
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");

    format!("# SnapLingo 收集夹\n\n共 {} 条\n\n{body}\n", items.len())
}
