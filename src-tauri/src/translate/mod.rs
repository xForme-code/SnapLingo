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
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::config;

/// 云端引擎的时间预算，按引擎类型分开。
///
/// 传统翻译 API（Google/有道/百度/DeepL）通常几百毫秒返回，4 秒足够；
/// 但 LLM 要生成 token，2~8 秒是常态——拿 4 秒去卡它，等于每次都判超时、
/// 回落本地、还进冷却，配了 Claude / OpenAI 也基本用不上。
fn budget_for(provider: &str) -> Duration {
    match provider {
        "claude" | "openai" => Duration::from_secs(20),
        _ => Duration::from_secs(4),
    }
}

/// 云端失败后的冷却期。
///
/// 断网时每次划词都去试一遍云端，等于每次都白等。记住上次失败，
/// 冷却期内直接走本地，用户体验才是「离线也顺手」。
const CLOUD_COOLDOWN: Duration = Duration::from_secs(60);

/// 被限流时的冷却。比「连不上」短得多——限流是瞬时状态，用 60 秒去躲它，
/// 等于把一次 429 放大成整整一分钟的不可用，代价比问题本身还大。
const RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(10);

/// 译文缓存。同一段文字、同一组语言、同一个引擎设置，结果不会变。
///
/// 存在的意义不只是快：免费的 Google 端点按 IP 限流，**请求数本身就是稀缺资源**。
/// 用户反复翻同一段（划错了重划、面板关了重开、收集夹里再看一遍）在真实使用里
/// 非常常见，每次都打一发网络请求是在自找 429。
///
/// 只缓存成功的结果——失败是瞬时状态（限流、超时、断网），缓存下来会把一次
/// 偶发失败变成持续失败，那比不缓存糟得多。
static TRANSLATION_CACHE: Lazy<Mutex<HashMap<CacheKey, Translation>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// 缓存上限。按条数卡而不是按字节：单条最长受 max_length 限制（默认 8000 字），
/// 200 条最坏也就几 MB，而常驻内存的工具不该为缓存吃掉更多。
const CACHE_LIMIT: usize = 200;

/// 缓存键。引擎要算进去——换了引擎译文风格完全不同，用户换引擎正是为了看到
/// 不一样的结果，这时候还给他上一个引擎的缓存等于换了个寂寞。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    text: String,
    source: String,
    target: String,
    provider: String,
}

fn cache_get(key: &CacheKey) -> Option<Translation> {
    TRANSLATION_CACHE.lock().ok()?.get(key).cloned()
}

fn cache_put(key: CacheKey, value: &Translation) {
    let Ok(mut map) = TRANSLATION_CACHE.lock() else {
        return;
    };
    // 满了就整个清空，不做 LRU。
    //
    // 看着粗暴，但这里的取舍很清楚：维护访问顺序要额外的数据结构和每次读的写锁，
    // 而缓存命中与否只影响快慢、不影响正确性。为一个纯优化项引入更复杂的并发
    // 结构不划算。
    if map.len() >= CACHE_LIMIT {
        log::debug!("译文缓存满（{CACHE_LIMIT} 条），清空重来");
        map.clear();
    }
    map.insert(key, value.clone());
}

/// 冷却状态**按引擎分开记**。
///
/// 共享一个全局冷却会串味：Google 挂了之后切到有道，有道也被跳过 60 秒，
/// 用户会以为「换了引擎还是不行」。各记各的才对得上因果。
static CLOUD_DOWN_UNTIL: Lazy<Mutex<HashMap<String, Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// 自动选路选中的引擎，以及选中的时间。
///
/// 选一次要挨个试，有失败的话每个都要等超时，代价不小。选中之后记住，
/// 后续直接用它；它进冷却了才重新选。
static ROUTE_WINNER: Lazy<Mutex<Option<(String, Instant)>>> = Lazy::new(|| Mutex::new(None));

/// 记住的选路结果多久重新评估一次。
///
/// 不能永久记住：用户可能从公司网络回到家、或者刚挂上代理，
/// 环境变了就该重新选。
const ROUTE_TTL: Duration = Duration::from_secs(600);

/// 自动选路的候选顺序。
///
/// 排序理由：国内直连的排前面——它们需要 Key，用户配了就说明有意愿用，
/// 而且不挂代理也能通；然后是免配置的 Google；最后才是按量付费的 LLM。
/// 这样在墙内墙外都能自己找到通的那条，用户不需要理解「我算国内还是国外网络」。
const ROUTE_ORDER: &[&str] = &["youdao", "baidu", "google", "deepl", "openai", "claude", "libre"];

/// 当前配置下，哪些云端引擎是可用的（填了 Key / 填了地址）
fn configured_cloud_engines(cfg: &config::Config) -> Vec<&'static str> {
    ROUTE_ORDER
        .iter()
        .copied()
        .filter(|id| match *id {
            "google" => true, // 免配置
            "youdao" => {
                !cfg.youdao_app_key.trim().is_empty() && !cfg.youdao_app_secret.trim().is_empty()
            }
            "baidu" => !cfg.baidu_app_id.trim().is_empty() && !cfg.baidu_secret.trim().is_empty(),
            "deepl" => !cfg.deepl_api_key.trim().is_empty(),
            "claude" => !cfg.claude_api_key.trim().is_empty(),
            "openai" => {
                !cfg.openai_api_key.trim().is_empty()
                    || cfg.openai_base_url.contains("localhost")
                    || cfg.openai_base_url.contains("127.0.0.1")
            }
            // 默认地址不算「已配置」：libre_url 出厂就是 localhost:5555，
            // 按「非空」判断的话它永远在候选里，自动选路每次都去试一个
            // 根本没跑起来的本地服务。真在本机跑了 LibreTranslate 的用户
            // 可以直接把引擎选成它，那条路不受这里影响。
            "libre" => {
                let url = cfg.libre_url.trim();
                !url.is_empty() && url != config::default_libre_url()
            }
            _ => false,
        })
        .collect()
}

fn remembered_route() -> Option<String> {
    let slot = ROUTE_WINNER.lock().ok()?;
    let (id, at) = slot.as_ref()?;
    if at.elapsed() < ROUTE_TTL && !cloud_in_cooldown(id) {
        Some(id.clone())
    } else {
        None
    }
}

fn remember_route(id: &str) {
    if let Ok(mut slot) = ROUTE_WINNER.lock() {
        *slot = Some((id.to_string(), Instant::now()));
    }
}

fn cloud_in_cooldown(provider: &str) -> bool {
    CLOUD_DOWN_UNTIL
        .lock()
        .ok()
        .and_then(|map| map.get(provider).copied())
        .is_some_and(|until| Instant::now() < until)
}

fn note_cloud_down(provider: &str) {
    note_cloud_down_for(provider, CLOUD_COOLDOWN);
}

/// 按失败原因决定躲多久。
///
/// 「连不上」多半要持续一段时间（断网、被墙、服务挂了），躲久一点省得白等；
/// 「被限流」下一秒就可能恢复，躲太久纯属自伤。
fn note_cloud_down_by(provider: &str, err: &anyhow::Error) {
    let text = err.to_string();
    let cooldown = if text.contains("429") {
        log::debug!("{provider} 是被限流，用短冷却");
        RATE_LIMIT_COOLDOWN
    } else {
        CLOUD_COOLDOWN
    };
    note_cloud_down_for(provider, cooldown);
}

fn note_cloud_down_for(provider: &str, cooldown: Duration) {
    if let Ok(mut map) = CLOUD_DOWN_UNTIL.lock() {
        map.insert(provider.to_string(), Instant::now() + cooldown);
    }
}

fn note_cloud_ok(provider: &str) {
    if let Ok(mut map) = CLOUD_DOWN_UNTIL.lock() {
        map.remove(provider);
    }
}

/// 这个错误是「网络不通」还是「配置不对」？
///
/// 只有网络类问题才该进冷却并静默回落。Key 填错、额度用尽这类配置问题
/// 必须原样告诉用户——否则他看到的是「已回落本地」，永远不知道自己的 Key 有问题。
fn is_transient(err: &anyhow::Error) -> bool {
    let text = err.to_string();
    let config_problem = [
        "未配置", "API Key", "密钥", "401", "403", "invalid", "Invalid",
        "unauthorized", "Unauthorized", "not authorized", "余额", "额度",
        "错误码", "不支持",
    ]
    .iter()
    .any(|marker| text.contains(marker));
    !config_problem
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

pub fn list_providers(app: &tauri::AppHandle) -> Vec<ProviderInfo> {
    let cfg = config::get();
    vec![
        ProviderInfo {
            id: "auto",
            label: "自动选择（推荐）",
            needs_key: false,
            available: true,
            note: "挨个试你配置好的云端引擎，谁通用谁，并记住结果。不用自己判断当前是国内还是国际网络。全都不通时回落离线翻译。",
        },
        ProviderInfo {
            id: "system",
            label: "系统翻译（离线）",
            needs_key: false,
            // 不能只看系统是不是 macOS：翻译 helper 需要 macOS 15+，
            // 低版本构建时会被跳过。这里查它是否真的在，否则用户会选到一个
            // 必然失败的引擎（而且显式选离线引擎时不会回落云端）。
            available: system::sidecar_available(app),
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

    // 长度上限要卡在**统一入口**，不能只卡在鼠标划选那条链上。
    // 快捷键翻译、截图翻译、收集夹批量翻译走的都是这里——不卡的话，
    // 一次误选整篇文档就会变成一次昂贵且漫长的请求。
    let length = raw.chars().count();
    if length > cfg.max_length {
        return Err(anyhow!(
            "选中内容太长（{length} 字，上限 {}）。可在设置里调整上限。",
            cfg.max_length
        ));
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

    // 命中缓存就直接返回，一发网络请求都不打。
    //
    // 键要用 prepared 而不是原始 text：标识符拆词的开关会改变实际送去翻译的内容，
    // 用原文当键的话，改了这个开关还会拿到旧结果。
    let cache_key = CacheKey {
        text: prepared.clone(),
        source: source_lang.clone(),
        target: target_lang.clone(),
        provider: provider_id.clone(),
    };
    if let Some(hit) = cache_get(&cache_key) {
        log::debug!("译文缓存命中，跳过请求");
        return Ok(hit);
    }

    let result = route(app, &cfg, &prepared, &source_lang, &target_lang, &provider_id).await;

    // **只在唯一出口写缓存**。选路里有五六个 return Ok，逐个补写迟早漏掉一个，
    // 而漏掉的那条路会表现成「这个引擎从来不走缓存」——没人会注意到。
    if let Ok(ref translation) = result {
        cache_put(cache_key, translation);
    }
    result
}

/// 挑一条路把这段文字翻出来。
///
/// 从 translate() 里拆出来，是为了让缓存只有一个写入点：这里面有好几个
/// 成功返回的分支，散着写缓存必然漏。
async fn route(
    app: &tauri::AppHandle,
    cfg: &config::Config,
    prepared: &str,
    source_lang: &str,
    target_lang: &str,
    provider_id: &str,
) -> Result<Translation> {
    // 用户明确选了某个离线引擎：那就只用本地，没有回落云端的必要
    if provider_id == "system" || provider_id == "opus" {
        return dispatch(app, provider_id, prepared, source_lang, target_lang).await;
    }

    // 自动选路：挨个试已配置的云端引擎，谁先成功就记住谁。
    //
    // 存在的意义：用户不该需要理解「我现在算国内网络还是国际网络」。
    // 墙内 Google 不通、墙外有道要绕远，与其让人自己判断，不如程序去试。
    if provider_id == "auto" {
        return auto_route(app, cfg, prepared, source_lang, target_lang).await;
    }

    // 否则走「联网优先、断网回落」：
    // 云端质量更好，能用就用；连不上就自动切本地，用户不需要知道发生了什么。
    let budget = budget_for(provider_id);
    // 云端为什么没成——一路带到最后的错误信息里。
    //
    // 以前这里什么都不记，最终错误写死成「网络似乎不通」。可失败原因常常
    // 根本不是断网（限流 429、超时、冷却期），用户照着那句话去查网络和代理，
    // 只会越查越远。
    let cloud_reason: Option<String>;
    if cloud_in_cooldown(provider_id) {
        log::debug!("{provider_id} 处于冷却期，直接走本地翻译");
        cloud_reason = Some(format!("{provider_id} 刚失败过，正在冷却，本次没有再试"));
    } else {
        match tokio::time::timeout(
            budget,
            dispatch(app, provider_id, prepared, source_lang, target_lang),
        )
        .await
        {
            Ok(Ok(translation)) => {
                note_cloud_ok(provider_id);
                return Ok(translation);
            }
            Ok(Err(err)) => {
                // 配置问题（Key 没填、Key 无效、额度用尽）不该被当成网络故障：
                // 静默回落会让用户永远查不出自己的 Key 有问题。原样抛给他。
                if !is_transient(&err) {
                    log::warn!("云端引擎 {provider_id} 配置有误，直接报给用户: {err}");
                    return Err(err);
                }
                log::warn!("云端引擎 {provider_id} 失败，回落本地: {err}");
                note_cloud_down_by(provider_id, &err);
                cloud_reason = Some(err.to_string());
            }
            Err(_) => {
                log::warn!("云端引擎 {provider_id} 超过 {budget:?} 未返回，回落本地");
                note_cloud_down(provider_id);
                cloud_reason = Some(format!("{provider_id} 超过 {budget:?} 没有返回"));
            }
        }
    }

    local_fallback(app, prepared, source_lang, target_lang, cloud_reason).await
}

/// 自动选路：按候选顺序试，第一个成功的就是这一轮的答案。
async fn auto_route(
    app: &tauri::AppHandle,
    cfg: &config::Config,
    prepared: &str,
    source_lang: &str,
    target_lang: &str,
) -> Result<Translation> {
    // 上次选中的还在有效期内、也没进冷却，直接用，省下挨个试的开销
    if let Some(id) = remembered_route() {
        match tokio::time::timeout(
            budget_for(&id),
            dispatch(app, &id, prepared, source_lang, target_lang),
        )
        .await
        {
            Ok(Ok(translation)) => return Ok(translation),
            Ok(Err(err)) => {
                log::warn!("自动选路：记住的 {id} 失败了，重新选: {err}");
                note_cloud_down(&id);
            }
            Err(_) => {
                log::warn!("自动选路：记住的 {id} 超时，重新选");
                note_cloud_down(&id);
            }
        }
    }

    let candidates = configured_cloud_engines(cfg);
    log::debug!("自动选路候选: {candidates:?}");

    for id in candidates {
        if cloud_in_cooldown(id) {
            continue;
        }
        match tokio::time::timeout(
            budget_for(id),
            dispatch(app, id, prepared, source_lang, target_lang),
        )
        .await
        {
            Ok(Ok(translation)) => {
                log::info!("自动选路：选中 {id}");
                remember_route(id);
                note_cloud_ok(id);
                return Ok(translation);
            }
            Ok(Err(err)) => {
                // 配置错误不该让这个引擎被反复重试，但也不该中断整轮选路——
                // 用户可能配了三个引擎，其中一个 Key 填错，剩下两个还能用
                log::debug!("自动选路：{id} 不通（{err}）");
                note_cloud_down_by(id, &err);
            }
            Err(_) => {
                log::debug!("自动选路：{id} 超时");
                note_cloud_down(id);
            }
        }
    }

    log::info!("自动选路：所有云端引擎都不通，回落本地");
    local_fallback(
        app,
        prepared,
        source_lang,
        target_lang,
        Some("已配置的云端引擎都没能返回结果".into()),
    )
    .await
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
    // 云端为什么没成。None 表示压根没试（用户直接选了离线引擎）。
    cloud_reason: Option<String>,
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
            Ok(translation) => {
                log::info!("本次由系统翻译完成（{source_lang}→{target_lang}）");
                return Ok(translation);
            }
            Err(err) => {
                log::info!("系统翻译可用但这次失败了: {err}");
                err
            }
        }
    } else {
        // 用 INFO 而不是 DEBUG：这一步直接决定用户拿到哪个引擎的译文，
        // 而正式版只记 INFO——出问题时如果这行不在日志里，就只能靠猜。
        // 语言对也要记：光说「语言包未就绪」，不知道问的是哪一对，无从核对。
        log::info!("系统翻译语言包未就绪（{pack:?}，{source_lang}→{target_lang}），改用离线模型");
        anyhow!("{}", system::NEEDS_DOWNLOAD)
    };

    // OPUS-MT 模型已经下载好的话，它比「让用户去下系统语言包」更直接可用
    match dispatch(app, "opus", prepared, source_lang, target_lang).await {
        Ok(translation) => {
            log::info!("本次由离线模型完成（{source_lang}→{target_lang}）");
            return Ok(translation);
        }
        Err(opus_err) => {
            log::info!("离线模型也不可用: {opus_err}");
        }
    }

    // 两条本地路都不成。
    let cloud_note = cloud_reason.unwrap_or_else(|| "没有试云端".into());

    // 优先透出「本地缺语言资源」——那是用户自己能解决的，前端靠这个标记显示引导。
    // 但**必须同时说清云端为什么没成**：只说「要下语言包」，用户会理所当然地想
    // 「我明明联着网，为什么要下语言包」——他不知道云端已经先失败了一次。
    if system_err.to_string().contains(system::NEEDS_DOWNLOAD) {
        return Err(anyhow!(
            "{}｜云端没成功：{cloud_note}；本机也没有这个语言方向的离线资源",
            system::NEEDS_DOWNLOAD
        ));
    }
    Err(anyhow!(
        "翻译失败。云端：{cloud_note}；本地：{system_err}"
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

#[cfg(test)]
mod cooldown_tests {
    use super::*;

    #[test]
    fn llm_gets_a_longer_budget_than_plain_apis() {
        // LLM 要生成 token，几秒是常态；拿传统 API 的预算去卡它会每次都超时
        assert!(budget_for("claude") > budget_for("google"));
        assert!(budget_for("openai") > budget_for("youdao"));
        assert_eq!(budget_for("google"), budget_for("baidu"));
    }

    #[test]
    fn cooldown_is_per_provider() {
        note_cloud_down("google");
        assert!(cloud_in_cooldown("google"));
        // 一个引擎挂掉不该连累另一个
        assert!(!cloud_in_cooldown("youdao"));
        note_cloud_ok("google");
        assert!(!cloud_in_cooldown("google"));
    }

    #[test]
    fn rate_limit_gets_a_shorter_cooldown_than_being_unreachable() {
        // 429 是瞬时状态，用 60 秒去躲它等于把一次限流放大成一分钟不可用
        assert!(RATE_LIMIT_COOLDOWN < CLOUD_COOLDOWN);

        note_cloud_down_by("t-429", &anyhow!("Google 翻译失败：HTTP 429。"));
        note_cloud_down_by("t-dead", &anyhow!("error sending request for url"));

        let until = |id: &str| CLOUD_DOWN_UNTIL.lock().unwrap().get(id).copied().unwrap();
        assert!(
            until("t-429") < until("t-dead"),
            "被限流的冷却必须比连不上的短"
        );

        note_cloud_ok("t-429");
        note_cloud_ok("t-dead");
    }

    #[test]
    fn config_errors_are_not_treated_as_network_failures() {
        assert!(!is_transient(&anyhow!("未配置 Claude API Key，请到设置里填写。")));
        assert!(!is_transient(&anyhow!("百度翻译失败（错误码 52003）：Unauthorized")));
        assert!(!is_transient(&anyhow!("Google 翻译失败：HTTP 403")));
        // 网络类问题才该静默回落并进冷却
        assert!(is_transient(&anyhow!("error sending request for url")));
        assert!(is_transient(&anyhow!("下载中断: connection reset")));
    }
}

#[cfg(test)]
mod cache_tests {
    use super::*;

    fn key(text: &str, source: &str, target: &str, provider: &str) -> CacheKey {
        CacheKey {
            text: text.into(),
            source: source.into(),
            target: target.into(),
            provider: provider.into(),
        }
    }

    fn sample(text: &str) -> Translation {
        Translation {
            text: text.into(),
            provider: "测试".into(),
            target: "en".into(),
            detected_source: None,
        }
    }

    /// 缓存键漏掉任何一个维度，都会表现成「改了设置还拿到旧结果」——
    /// 而且用户多半不会怀疑到缓存头上，只会觉得这个设置项没生效。
    #[test]
    fn key_distinguishes_every_dimension() {
        let base = key("hello", "auto", "zh-CN", "google");
        cache_put(base.clone(), &sample("你好"));

        assert_eq!(cache_get(&base).unwrap().text, "你好");
        // 换目标语言
        assert!(cache_get(&key("hello", "auto", "ja", "google")).is_none());
        // 换源语言
        assert!(cache_get(&key("hello", "en", "zh-CN", "google")).is_none());
        // 换引擎——用户换引擎正是为了看到不一样的译文
        assert!(cache_get(&key("hello", "auto", "zh-CN", "deepl")).is_none());
        // 换原文
        assert!(cache_get(&key("hallo", "auto", "zh-CN", "google")).is_none());
    }

    #[test]
    fn cache_is_bounded() {
        // 常驻后台的工具不能让缓存无限长。满了整个清空是有意的取舍：
        // 命中与否只影响快慢，不值得为它引入 LRU 那套并发结构。
        for i in 0..(CACHE_LIMIT + 5) {
            cache_put(key(&format!("t{i}"), "auto", "en", "google"), &sample("x"));
        }
        assert!(TRANSLATION_CACHE.lock().unwrap().len() <= CACHE_LIMIT);
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;

    fn cfg_with(youdao: bool, deepl: bool) -> config::Config {
        let mut c = config::Config::default();
        if youdao {
            c.youdao_app_key = "k".into();
            c.youdao_app_secret = "s".into();
        }
        if deepl {
            c.deepl_api_key = "k".into();
        }
        c.libre_url = String::new(); // 默认指向 localhost，测试里排除掉
        c
    }

    #[test]
    fn only_configured_engines_are_candidates() {
        // 没配任何 Key 时，只有免配置的 Google 可选
        assert_eq!(configured_cloud_engines(&cfg_with(false, false)), vec!["google"]);
    }

    #[test]
    fn domestic_engines_are_tried_before_google() {
        // 用户特意配了有道，说明有「不挂代理也要能用」的诉求，该优先试
        let candidates = configured_cloud_engines(&cfg_with(true, false));
        let youdao = candidates.iter().position(|id| *id == "youdao").unwrap();
        let google = candidates.iter().position(|id| *id == "google").unwrap();
        assert!(youdao < google, "国内直连引擎应排在 Google 之前");
    }

    #[test]
    fn paid_llm_engines_come_last() {
        let candidates = configured_cloud_engines(&cfg_with(true, true));
        let deepl = candidates.iter().position(|id| *id == "deepl").unwrap();
        let google = candidates.iter().position(|id| *id == "google").unwrap();
        assert!(google < deepl, "免费的应排在按量付费的之前");
    }
}
