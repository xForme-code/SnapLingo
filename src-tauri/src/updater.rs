//! 自动更新。
//!
//! 走 Rust 侧而不是前端：前端调用需要额外配 updater 的权限，而这件事完全
//! 不需要前端参与——它没有界面，只有一个系统对话框。
//!
//! 更新包由 `tauri signer` 的私钥签名，公钥编译进应用。下载下来的包验签
//! 不通过就直接丢弃，所以即使 Release 被人替换了文件也装不进去。

use std::sync::atomic::{AtomicBool, Ordering};

use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

/// 检查更新的时限。
///
/// 不设的话连不上时会一直挂着，用户点完「检查更新…」什么反应都没有，
/// 只能猜是不是坏了。GitHub 在部分网络环境下本来就时通时不通，
/// 与其无限期等待，不如快点失败并说清原因。
const CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// 启动后延迟多久做第一次检查。
///
/// 不在启动瞬间查：那会儿用户可能正等着划词，网络请求和对话框都是打扰。
/// 而且启动初期网络往往还没就绪（尤其代理刚拉起来时）。
const STARTUP_DELAY: std::time::Duration = std::time::Duration::from_secs(20);

/// 之后每隔多久再查一次。
///
/// 这是个常驻菜单栏的工具，用户开着几天几周很正常。只在启动时查一次的话，
/// 他永远不会知道有新版本——除非哪天想起来手动点一下。
const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

/// 后台的定期静默检查：有更新才出声，没有就什么都不做。
///
/// 开关是**每轮都读**的，不是启动时读一次。这样用户在设置里打开自动检查后
/// 不需要重启——之前正是这样：启动时开关是关的，后来打开了，程序却毫不知情。
pub fn start_background_checks(app: &AppHandle) {
    // 记下启动时开关的真实状态，供 setting_just_enabled 比对。
    // 必须在这里记：等到用户保存设置时再读就晚了——那时内存里已经是新值。
    ENABLED.store(crate::config::get().auto_check_update, Ordering::Relaxed);

    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        loop {
            if crate::config::get().auto_check_update {
                if let Err(err) = check(&handle, false).await {
                    // 静默检查失败不值得打扰用户——断网、Release 还没发都会走到这
                    log::debug!("定期检查更新失败（忽略）: {err}");
                }
            }
            tokio::time::sleep(CHECK_INTERVAL).await;
        }
    });
}

/// 自动检查开关当前的状态。用来识别「关 → 开」这一次跳变。
static ENABLED: AtomicBool = AtomicBool::new(false);

/// 开关是不是刚从关变成开。
///
/// 只在这一次跳变时返回 true。不判断跳变的话，用户每保存一次设置
/// （改个快捷键、换个引擎）都会多查一次更新。
pub fn setting_just_enabled(now: bool) -> bool {
    let was = ENABLED.swap(now, Ordering::Relaxed);
    now && !was
}

/// 用户刚在设置里打开自动检查：立刻查一次。
///
/// 不这么做的话，他打开开关之后最多要等到下一个检查周期才有反应，
/// 而那可能是几小时后——看起来就像这个开关没生效。
pub fn check_after_enabling(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(err) = check(&handle, false).await {
            log::debug!("打开自动检查后的首次检查失败（忽略）: {err}");
        }
    });
}

/// 用户从托盘主动点「检查更新」：无论有没有都要给个回应，
/// 否则点了没反应会让人以为坏了。
pub fn check_manually(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(err) = check(&handle, true).await {
            log::warn!("检查更新失败: {err}");
            notify(&handle, "检查更新失败", &explain(&err));
        }
    });
}

async fn check(app: &AppHandle, verbose: bool) -> tauri_plugin_updater::Result<()> {
    let update = app
        .updater_builder()
        .timeout(CHECK_TIMEOUT)
        .build()?
        .check()
        .await?;

    let Some(update) = update else {
        log::info!("检查更新：已是最新版本");
        if verbose {
            notify(app, "已是最新版本", &format!("当前版本 {}", app.package_info().version));
        }
        return Ok(());
    };

    log::info!("发现新版本 {}", update.version);
    let handle = app.clone();
    let version = update.version.clone();
    let notes = update.body.clone().unwrap_or_default();

    // 装不装是用户的决定：更新会替换正在运行的程序并重启，
    // 不该在他毫无察觉的情况下发生。
    let message = if notes.trim().is_empty() {
        format!("发现新版本 {version}，是否现在更新？")
    } else {
        let brief: String = notes.chars().take(300).collect();
        format!("发现新版本 {version}\n\n{brief}\n\n是否现在更新？")
    };

    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog()
        .message(message)
        .title("SnapLingo 有新版本")
        .buttons(MessageDialogButtons::OkCancelCustom(
            "更新并重启".into(),
            "以后再说".into(),
        ))
        .show(move |confirmed| {
            let _ = tx.send(confirmed);
        });

    let confirmed = tokio::task::spawn_blocking(move || rx.recv().unwrap_or(false))
        .await
        .unwrap_or(false);

    if !confirmed {
        log::info!("用户选择稍后更新");
        return Ok(());
    }

    log::info!("开始下载更新 {version}");
    let mut downloaded = 0usize;
    update
        .download_and_install(
            |chunk, total| {
                downloaded += chunk;
                if let Some(total) = total {
                    log::debug!("更新下载中 {downloaded}/{total}");
                }
            },
            || log::info!("更新下载完成，准备安装"),
        )
        .await?;

    // 安装完必须重启才能生效
    log::info!("更新已安装，重启应用");
    handle.restart();
}

/// 把底层报错翻译成用户能照着做点什么的话。
///
/// 原样透出的话，用户看到的是「error sending request for url
/// (https://github.com/.../latest.json)」——英文、带一串他不关心的地址、
/// 而且完全没说该怎么办。更新地址在 GitHub 上，部分网络环境下连不通是常事，
/// 这时候需要的是「检查网络或代理」，不是一段技术细节。
fn explain(err: &tauri_plugin_updater::Error) -> String {
    let raw = err.to_string();
    let networkish = raw.contains("error sending request")
        || raw.contains("timed out")
        || raw.contains("dns")
        || raw.contains("connect");

    if networkish {
        // 分行拼接而不是用反斜杠续行：续行在某些编辑/生成流程里会丢，
        // 丢了就把源码的缩进空格原样带进弹窗，显示成一串莫名其妙的空隙
        [
            "连不上 GitHub（更新信息放在那里）。",
            "",
            "请检查网络或代理设置后重试。",
            "也可以到 github.com/xForme-code/SnapLingo/releases 手动下载。",
        ]
        .join("\n")
    } else {
        raw
    }
}

fn notify(app: &AppHandle, title: &str, message: &str) {
    use tauri_plugin_dialog::DialogExt;
    app.dialog().message(message).title(title).show(|_| {});
}
