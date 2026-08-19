use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractGroup {
    pub label: &'static str,
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractResult {
    pub groups: Vec<ExtractGroup>,
    pub markdown: String,
    pub total: usize,
}

/// 纯本地的结构化提取规则：不联网、不花钱、不外发内容。
/// 顺序即展示顺序，先具体后宽泛，避免宽规则抢走匹配。
static PATTERNS: Lazy<Vec<(&'static str, Regex)>> = Lazy::new(|| {
    let rules: &[(&'static str, &str)] = &[
        ("链接", r#"https?://[^\s<>"'）)】\]，,。;；]+"#),
        ("邮箱", r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9.\-]+\.[A-Za-z]{2,}"),
        ("手机号", r"(?:\+?86[\s-]?)?1[3-9]\d{9}"),
        (
            "座机 / 国际号码",
            r"(?:\+\d{1,3}[\s-]?)?\(?\d{3,4}\)?[\s-]\d{3,4}[\s-]\d{4}",
        ),
        ("IP 地址", r"\b(?:\d{1,3}\.){3}\d{1,3}\b(?::\d{1,5})?"),
        ("日期", r"\d{4}\s*[-/年]\s*\d{1,2}\s*[-/月]\s*\d{1,2}\s*日?"),
        (
            "金额",
            r"(?:[$¥€£￥]\s?\d[\d,]*(?:\.\d+)?|\d[\d,]*(?:\.\d+)?\s?(?:元|美元|人民币|万元|亿元|万|亿))",
        ),
        ("版本号", r"\bv?\d+\.\d+(?:\.\d+)+(?:-[\w.]+)?\b"),
        ("文件路径", r"(?:[A-Za-z]:\\[^\s，,。;；]+|(?:/[\w.\-]+){2,})"),
    ];

    rules
        .iter()
        .map(|(label, pattern)| {
            let regex = Regex::new(pattern)
                .unwrap_or_else(|err| panic!("提取规则「{label}」正则非法: {err}"));
            (*label, regex)
        })
        .collect()
});

pub fn extract_local(text: &str) -> ExtractResult {
    let mut groups: Vec<ExtractGroup> = Vec::new();

    for (label, regex) in PATTERNS.iter() {
        // BTreeSet 顺带去重并保持稳定顺序
        let found: BTreeSet<String> = regex
            .find_iter(text)
            .map(|m| m.as_str().trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        if !found.is_empty() {
            groups.push(ExtractGroup {
                label,
                items: found.into_iter().collect(),
            });
        }
    }

    // 什么都没匹配到时，退化成「去重后的非空行」，至少给用户一个可用的结果
    if groups.is_empty() {
        let lines: Vec<String> = {
            let mut seen = BTreeSet::new();
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .filter(|l| seen.insert(l.to_string()))
                .map(str::to_string)
                .collect()
        };
        if !lines.is_empty() {
            groups.push(ExtractGroup {
                label: "文本行",
                items: lines,
            });
        }
    }

    let total = groups.iter().map(|g| g.items.len()).sum();
    let markdown = groups
        .iter()
        .map(|g| {
            let items = g
                .items
                .iter()
                .map(|i| format!("- {i}"))
                .collect::<Vec<_>>()
                .join("\n");
            format!("## {}\n{items}", g.label)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    ExtractResult {
        groups,
        markdown,
        total,
    }
}

#[cfg(test)]
mod tests {
    use super::extract_local;

    #[test]
    fn finds_common_entities() {
        let text = "联系 support@snaplingo.dev 或 13800138000，\
                    文档 https://example.com/docs 服务器 192.168.1.100:8080，\
                    2026年8月15日 发布 v1.2.3，费用 ¥1,299.00";
        let result = extract_local(text);
        let labels: Vec<_> = result.groups.iter().map(|g| g.label).collect();

        assert!(labels.contains(&"邮箱"));
        assert!(labels.contains(&"手机号"));
        assert!(labels.contains(&"链接"));
        assert!(labels.contains(&"IP 地址"));
        assert!(labels.contains(&"日期"));
        assert!(labels.contains(&"版本号"));
        assert!(labels.contains(&"金额"));
    }

    #[test]
    fn falls_back_to_lines() {
        let result = extract_local("第一行\n第二行\n第一行");
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.groups[0].label, "文本行");
        // 重复行应被去掉
        assert_eq!(result.groups[0].items.len(), 2);
    }
}
