//! 权限自检。
//!
//! 取词依赖「辅助功能」，截图翻译依赖「屏幕录制」。
//! 没有权限时 enigo / rdev 只会静默失败，用户完全不知道发生了什么，
//! 所以启动时主动检查一次并给出可操作的指引。

use tauri::AppHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Accessibility,
    ScreenRecording,
}

impl Permission {
    fn label(self) -> &'static str {
        match self {
            Permission::Accessibility => "辅助功能",
            Permission::ScreenRecording => "屏幕录制",
        }
    }

    fn why(self) -> &'static str {
        match self {
            Permission::Accessibility => "读取你在其它 App 里选中的文字（划词翻译的前提）",
            Permission::ScreenRecording => "截取屏幕区域做文字识别",
        }
    }

    #[cfg(target_os = "macos")]
    fn settings_url(self) -> &'static str {
        match self {
            Permission::Accessibility => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            }
            Permission::ScreenRecording => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            }
        }
    }
}

// ---------------------------------------------------------------- macOS

#[cfg(target_os = "macos")]
mod sys {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    pub fn accessibility_granted() -> bool {
        // AXIsProcessTrusted 不带 prompt，纯查询，不会弹窗打扰用户
        unsafe { AXIsProcessTrusted() }
    }

    pub fn screen_recording_granted() -> bool {
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub fn request_screen_recording() -> bool {
        // 这个会触发系统弹窗，首次调用时才有效
        unsafe { CGRequestScreenCaptureAccess() }
    }
}

#[cfg(target_os = "macos")]
pub fn is_granted(permission: Permission) -> bool {
    match permission {
        Permission::Accessibility => sys::accessibility_granted(),
        Permission::ScreenRecording => sys::screen_recording_granted(),
    }
}

/// 触发系统的屏幕录制授权弹窗（辅助功能没有等价的静默请求接口）
#[cfg(target_os = "macos")]
pub fn request_screen_recording() -> bool {
    sys::request_screen_recording()
}

#[cfg(not(target_os = "macos"))]
pub fn is_granted(_permission: Permission) -> bool {
    // Windows 不需要额外授权；Linux 的限制来自 Wayland，另行提示
    true
}

#[cfg(not(target_os = "macos"))]
pub fn request_screen_recording() -> bool {
    true
}

// ---------------------------------------------------------------- 启动引导

/// 直接打开对应的系统设置页
pub fn open_settings_page(app: &AppHandle, permission: Permission) {
    #[cfg(target_os = "macos")]
    {
        use tauri_plugin_opener::OpenerExt;
        let _ = app.opener().open_url(permission.settings_url(), None::<&str>);
    }
    let _ = (app, permission);
}

/// 每次运行最多弹一次权限说明，避免反复骚扰。
static PROMPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// 启动时的检查。
///
/// 只在「首次运行」时主动提示；之后即使没授权也保持安静——
/// 真正需要权限的时候会由 `prompt_if_missing` 在功能失败的当下提示，
/// 那时候用户正好知道自己想干什么，提示才有意义。
pub fn check_on_startup(app: &AppHandle, first_run: bool) {
    #[cfg(target_os = "linux")]
    {
        if is_wayland() {
            warn_wayland(app);
        }
    }

    #[cfg(target_os = "macos")]
    {
        if first_run && !is_granted(Permission::Accessibility) {
            prompt_if_missing(app, Permission::Accessibility);
        }
    }

    let _ = (app, first_run);
}

/// 功能真正失败时才提示，且一次运行只提示一次。
pub fn prompt_if_missing(app: &AppHandle, permission: Permission) -> bool {
    if is_granted(permission) {
        return true;
    }
    use std::sync::atomic::Ordering;
    if PROMPTED.swap(true, Ordering::Relaxed) {
        log::warn!("缺少「{}」权限（本次运行已提示过，不再重复弹窗）", permission.label());
        return false;
    }

    #[cfg(target_os = "macos")]
    prompt(app, permission);

    let _ = app;
    false
}

#[cfg(target_os = "macos")]
fn prompt(app: &AppHandle, permission: Permission) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

    let handle = app.clone();
    let message = format!(
        "SnapLingo 需要「{}」权限，用于{}。\n\n\
         点击「打开设置」后，在列表里勾选 SnapLingo，然后重新启动本程序。",
        permission.label(),
        permission.why()
    );

    app.dialog()
        .message(message)
        .title(format!("需要开启「{}」权限", permission.label()))
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            "打开设置".into(),
            "稍后".into(),
        ))
        .show(move |confirmed| {
            if confirmed {
                use tauri_plugin_opener::OpenerExt;
                let _ = handle.opener().open_url(permission.settings_url(), None::<&str>);
            }
        });
}

#[cfg(target_os = "linux")]
fn is_wayland() -> bool {
    std::env::var("WAYLAND_DISPLAY").is_ok()
        || std::env::var("XDG_SESSION_TYPE").map(|v| v == "wayland").unwrap_or(false)
}

#[cfg(target_os = "linux")]
fn warn_wayland(app: &AppHandle) {
    use tauri_plugin_dialog::{DialogExt, MessageDialogKind};

    // Wayland 的安全模型不允许应用监听全局输入，这是协议层面的限制，
    // 不是本程序的 bug，所以要明确告诉用户，而不是让它静默失效。
    app.dialog()
        .message(
            "检测到当前是 Wayland 会话。\n\n\
             Wayland 出于安全考虑不允许程序监听全局鼠标事件，\
             因此「划词自动触发」无法工作，全局快捷键也可能失效。\n\n\
             解决办法（任选其一）：\n\
             · 登录时选择 “Ubuntu on Xorg” 会话\n\
             · 安装 ydotool 并启动 ydotoold 服务\n\n\
             在此之前，请使用托盘菜单里的功能。",
        )
        .title("Wayland 会话功能受限")
        .kind(MessageDialogKind::Warning)
        .show(|_| {});
}
