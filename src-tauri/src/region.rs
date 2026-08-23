//! Windows / Linux 的截图框选遮罩。
//!
//! macOS 不走这里：它直接复用系统自带的 `screencapture -i`，用户对那套框选 UI
//! 已经很熟，我们没必要自己画一个。其它平台没有等价物，只能自绘。
//!
//! 链路是：铺一个盖满当前显示器的透明置顶窗口 → 用户拖框 → 前端把选区回传
//! → 这里换算成全局物理坐标 → `capture::capture_region` 真正截图。

use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use serde::Deserialize;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, PhysicalPosition, PhysicalSize};
use tokio::sync::oneshot;

const LABEL: &str = "region";

/// 用户框出来的区域，**全局物理像素**——`capture_region` 要的就是这个。
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// 遮罩要盖住的那块屏幕。
struct Screen {
    /// 左上角在虚拟桌面里的位置，物理像素
    origin: (i32, i32),
    /// 分辨率，物理像素
    size: (u32, u32),
    scale: f64,
}

/// 前端回传的选区：**CSS 像素**，相对遮罩窗口左上角。
///
/// 和 Rect 单位不同，别混用——差一个缩放系数，在 150% 缩放的机器上
/// 截出来的区域会比框的小三分之一，而在 100% 的机器上完全正常，
/// 是那种「我这儿没问题」的经典 bug。
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// 等待前端回传的那一端。同一时刻只可能有一次框选。
static PENDING: Lazy<Mutex<Option<oneshot::Sender<Option<Selection>>>>> =
    Lazy::new(|| Mutex::new(None));

/// 框选超时。用户拉起遮罩又跑去干别的时，不能让整条链路（连同 OCR_BUSY 标志）
/// 永远卡住——那会表现成「截图翻译用一次就再也没反应了」。
const TIMEOUT: Duration = Duration::from_secs(60);

/// 前端把结果交回来。传 null 表示取消。
#[tauri::command]
pub fn region_result(selection: Option<Selection>) {
    if let Some(tx) = PENDING.lock().ok().and_then(|mut slot| slot.take()) {
        let _ = tx.send(selection);
    }
}

/// 拉起遮罩，等用户框一块区域出来。用户取消时返回 Ok(None)。
pub async fn pick(app: &AppHandle) -> Result<Option<Rect>> {
    let screen = target_monitor(app)?;

    let window = crate::windows::ensure(
        app,
        LABEL,
        "region.html",
        "SnapLingo 截图",
        // 尺寸随后按显示器物理尺寸重设，这里给什么都会被覆盖
        (100.0, 100.0),
        |b| {
            b.decorations(false)
                .resizable(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .transparent(true)
                .shadow(false)
                .focused(true)
        },
    )?;

    // 用物理单位摆放：逻辑单位要先除缩放，多显示器不同缩放时最容易在这儿算错
    window
        .set_position(PhysicalPosition::new(screen.origin.0, screen.origin.1))
        .map_err(|e| anyhow!("移动遮罩窗口失败: {e}"))?;
    window
        .set_size(PhysicalSize::new(screen.size.0, screen.size.1))
        .map_err(|e| anyhow!("调整遮罩窗口失败: {e}"))?;

    let (tx, rx) = oneshot::channel();
    {
        let mut slot = PENDING.lock().map_err(|_| anyhow!("框选状态锁损坏"))?;
        // 上一次没善终的话先把它了结掉，否则那边会一直挂着
        if let Some(stale) = slot.replace(tx) {
            let _ = stale.send(None);
        }
    }

    // 窗口是复用的，先让前端把上次的选框清掉再显示
    let _ = window.emit("region:reset", ());
    window.show().map_err(|e| anyhow!("显示遮罩窗口失败: {e}"))?;
    let _ = window.set_always_on_top(true);
    let _ = window.set_focus();

    let selection = match tokio::time::timeout(TIMEOUT, rx).await {
        Ok(Ok(value)) => value,
        // 发送端被丢弃（窗口被销毁之类），当作取消
        Ok(Err(_)) => None,
        Err(_) => {
            log::info!("框选超时，自动取消");
            PENDING.lock().ok().and_then(|mut slot| slot.take());
            None
        }
    };

    let _ = window.hide();

    let Some(sel) = selection else {
        return Ok(None);
    };

    // 遮罩自己还在屏幕上时截图，截到的是那层灰罩子。hide() 只是发出请求，
    // 合成器真正把它从画面上抹掉还要一两帧，所以这里必须等一下再截。
    tokio::time::sleep(Duration::from_millis(120)).await;

    Ok(to_physical(sel, screen.origin, screen.scale))
}

/// CSS 像素 → 全局物理像素。
///
/// 选区太小时返回 None：随手点一下也会产生一个 1×1 的「选区」，
/// 截出来的图 OCR 必然什么都认不出，不如当成取消。
fn to_physical(sel: Selection, origin: (i32, i32), scale: f64) -> Option<Rect> {
    let width = (sel.width * scale).round();
    let height = (sel.height * scale).round();
    if width < 8.0 || height < 8.0 {
        return None;
    }

    Some(Rect {
        x: origin.0 + (sel.x * scale).round() as i32,
        y: origin.1 + (sel.y * scale).round() as i32,
        width: width as u32,
        height: height as u32,
    })
}

/// 光标所在的那块显示器。
///
/// 不用 monitor_from_point：它吃的是逻辑坐标还是物理坐标，各平台并不一致，
/// 这里直接拿物理光标位置去比物理矩形，没有歧义。
fn target_monitor(app: &AppHandle) -> Result<Screen> {
    let cursor = app.cursor_position().ok();

    let monitors = app
        .available_monitors()
        .map_err(|e| anyhow!("读取显示器列表失败: {e}"))?;

    let hit = cursor.and_then(|c| {
        monitors.iter().find(|m| {
            let p = m.position();
            let s = m.size();
            c.x >= p.x as f64
                && c.x < (p.x + s.width as i32) as f64
                && c.y >= p.y as f64
                && c.y < (p.y + s.height as i32) as f64
        })
    });

    // 光标位置读不到、或落在所有显示器之外（多屏拔掉一块时会出现）就退回主屏
    let monitor = match hit {
        Some(m) => m.clone(),
        None => app
            .primary_monitor()
            .map_err(|e| anyhow!("读取主显示器失败: {e}"))?
            .or_else(|| monitors.first().cloned())
            .ok_or_else(|| anyhow!("没有可用的显示器"))?,
    };

    let p = monitor.position();
    let s = monitor.size();
    Ok(Screen {
        origin: (p.x, p.y),
        size: (s.width, s.height),
        scale: monitor.scale_factor(),
    })
}

#[cfg(test)]
mod tests {
    use super::{to_physical, Selection};

    fn sel(x: f64, y: f64, width: f64, height: f64) -> Selection {
        Selection { x, y, width, height }
    }

    #[test]
    fn maps_css_pixels_to_global_physical() {
        // 100% 缩放的主屏：CSS 像素和物理像素一比一，原点也是 0
        let r = to_physical(sel(10.0, 20.0, 300.0, 200.0), (0, 0), 1.0).unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (10, 20, 300, 200));
    }

    #[test]
    fn accounts_for_display_scaling() {
        // 150% 缩放：框出来 300 CSS 像素宽，实际是 450 个物理像素。
        // 少乘这一下的话，截出来的图会比用户框的小三分之一——
        // 而在 100% 的机器上完全正常，是最难复现的那类 bug。
        let r = to_physical(sel(10.0, 20.0, 300.0, 200.0), (0, 0), 1.5).unwrap();
        assert_eq!((r.x, r.y, r.width, r.height), (15, 30, 450, 300));
    }

    #[test]
    fn offsets_by_monitor_origin() {
        // 副屏摆在主屏右边：选区坐标是相对遮罩窗口的，必须加上显示器原点，
        // 否则不管在哪块屏上框，截到的都是主屏对应位置的内容
        let r = to_physical(sel(0.0, 0.0, 100.0, 100.0), (1920, -180), 1.0).unwrap();
        assert_eq!((r.x, r.y), (1920, -180));
    }

    #[test]
    fn rejects_accidental_click() {
        // 随手点一下也会产生一个极小的「选区」，截出来 OCR 什么都认不出
        assert!(to_physical(sel(100.0, 100.0, 2.0, 2.0), (0, 0), 1.0).is_none());
        // 高缩放下 5 CSS 像素其实有 10 个物理像素，不该被当成误触
        assert!(to_physical(sel(100.0, 100.0, 5.0, 5.0), (0, 0), 2.0).is_some());
    }
}
