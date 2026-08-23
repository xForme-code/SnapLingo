use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// 截图产物落在临时目录，OCR 读完就删
pub fn temp_image_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("snaplingo-{stamp}.png"))
}

/// 用户框选一块屏幕区域，返回截图文件路径。
/// 用户按 Esc 取消时返回 Ok(None)。
pub async fn select_region(app: &tauri::AppHandle) -> Result<Option<PathBuf>> {
    select_region_platform(app).await
}

#[cfg(target_os = "macos")]
async fn select_region_platform(_app: &tauri::AppHandle) -> Result<Option<PathBuf>> {
    use tokio::process::Command;

    // 直接复用 macOS 自带的框选 UI（和 ⌘⇧4 是同一个）：
    // 用户已经很熟悉，而且不需要我们自己画透明遮罩窗口。
    //   -i 交互式框选   -x 不播快门声   -o 不要窗口阴影
    let path = temp_image_path();
    let status = Command::new("screencapture")
        .args(["-i", "-x", "-o"])
        .arg(&path)
        .status()
        .await
        .map_err(|e| anyhow!("调用 screencapture 失败: {e}"))?;

    if !status.success() {
        return Err(anyhow!(
            "截图失败。请到 系统设置 → 隐私与安全性 → 屏幕录制 中允许 SnapLingo。"
        ));
    }

    // 用户按 Esc 取消时，screencapture 不会生成文件
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(path))
}

#[cfg(not(target_os = "macos"))]
async fn select_region_platform(app: &tauri::AppHandle) -> Result<Option<PathBuf>> {
    // 没有系统级的框选 UI，用自绘遮罩（region.rs）拿到区域再截
    let Some(rect) = crate::region::pick(app).await? else {
        return Ok(None);
    };

    // 截图是同步的位图搬运，别占着异步运行时的工作线程
    tokio::task::spawn_blocking(move || capture_region(rect.x, rect.y, rect.width, rect.height))
        .await
        .map_err(|e| anyhow!("截图任务异常: {e}"))?
        .map(Some)
}

/// 按全局物理坐标截取一块区域并存成 PNG。
/// 供 Windows / Linux 的自绘遮罩使用；macOS 走 screencapture，用不到。
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn capture_region(x: i32, y: i32, width: u32, height: u32) -> Result<PathBuf> {
    use xcap::Monitor;

    if width == 0 || height == 0 {
        return Err(anyhow!("选区太小"));
    }

    let monitor = Monitor::from_point(x, y)
        .map_err(|e| anyhow!("找不到该坐标所在的显示器: {e}"))?;

    let origin_x = monitor.x().map_err(|e| anyhow!("读取显示器位置失败: {e}"))?;
    let origin_y = monitor.y().map_err(|e| anyhow!("读取显示器位置失败: {e}"))?;

    // capture_region 用的是相对该显示器左上角的坐标
    let image = monitor
        .capture_region(
            (x - origin_x).max(0) as u32,
            (y - origin_y).max(0) as u32,
            width,
            height,
        )
        .map_err(|e| anyhow!("截图失败: {e}"))?;

    let path = temp_image_path();
    image
        .save(&path)
        .map_err(|e| anyhow!("保存截图失败: {e}"))?;
    Ok(path)
}

/// OCR 读完后清理临时文件，避免截图残留在磁盘上
pub fn cleanup(path: &PathBuf) {
    if let Err(err) = std::fs::remove_file(path) {
        log::debug!("清理临时截图失败（可忽略）: {err}");
    }
}
