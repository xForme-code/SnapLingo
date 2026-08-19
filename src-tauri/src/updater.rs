//! 自动更新。
//!
//! 走 Rust 侧而不是前端：前端调用需要额外配 updater 的权限，而这件事完全
//! 不需要前端参与——它没有界面，只有一个系统对话框。
//!
//! 更新包由 `tauri signer` 的私钥签名，公钥编译进应用。下载下来的包验签
//! 不通过就直接丢弃，所以即使 Release 被人替换了文件也装不进去。

use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

/// 启动后延迟多久做静默检查。
///
/// 不在启动瞬间查：那会儿用户可能正等着划词，网络请求和对话框都是打扰。
/// 而且启动初期网络往往还没就绪（尤其代理刚拉起来时）。
const STARTUP_DELAY: std::time::Duration = std::time::Duration::from_secs(20);

/// 静默检查：有更新才出声，没有就什么都不做。
pub fn check_on_startup(app: &AppHandle) {
    // 用户关掉了就彻底不查：不发请求、不弹窗。
    // 托盘菜单里的「检查更新…」不受影响，想查随时手动查。
    if !crate::config::get().auto_check_update {
        log::info!("自动检查更新已关闭，跳过启动检查");
        return;
    }

    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        if let Err(err) = check(&handle, false).await {
            // 启动时的检查失败不值得打扰用户——断网、Release 还没发都会走到这
            log::debug!("启动时检查更新失败（忽略）: {err}");
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
            notify(&handle, "检查更新失败", &format!("{err}"));
        }
    });
}

async fn check(app: &AppHandle, verbose: bool) -> tauri_plugin_updater::Result<()> {
    let update = app.updater()?.check().await?;

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

fn notify(app: &AppHandle, title: &str, message: &str) {
    use tauri_plugin_dialog::DialogExt;
    app.dialog().message(message).title(title).show(|_| {});
}
