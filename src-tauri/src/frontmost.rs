//! 记住并恢复「划词那一刻的前台程序」。
//!
//! 为什么需要：把译文写回用户文档要靠模拟粘贴，而粘贴粘到哪个程序，取决于
//! 那一刻谁是前台。用户一点我们的气泡，前台就变成了 SnapLingo 自己——
//! 这时候粘贴，轻则粘进我们自己的窗口（什么也不会发生），重则粘进用户上一个
//! 碰过的文档里，把无关的文件改坏。
//!
//! 所以在**取词那一刻**（气泡还没出现、焦点还没动）把前台程序记下来，
//! 真要写回时先切回去。记不住就不写回——宁可退化成「已复制，请手动粘贴」。

use std::sync::Mutex;

use once_cell::sync::Lazy;

/// 上一次取词时的前台程序。每次取词覆盖。
static LAST: Lazy<Mutex<Option<Owner>>> = Lazy::new(|| Mutex::new(None));

/// 前台程序的标识。macOS 上是进程号，Windows 上是窗口句柄，
/// 各平台内容不同，外面不关心里面是什么。
///
/// 用 isize 而不是 i32：Windows 的 HWND 是指针宽度的，虽然实际值一直落在
/// 32 位内，但那是文档层面的「可互操作」约定，不是类型保证——按 i32 存就是
/// 在赌，赌输了是「切回了另一个窗口」这种极难复现的错。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Owner(isize);

/// 取词开始时调用，记下当前前台程序。
pub fn remember() {
    let owner = imp::current();
    if owner.is_none() {
        log::debug!("没能读到前台程序，本次不支持写回替换");
    }
    if let Ok(mut slot) = LAST.lock() {
        *slot = owner;
    }
}

/// 把焦点切回取词时的那个程序。切不回去就返回 false，调用方**必须**据此
/// 放弃写回——切不回去还硬粘贴，就是往错误的地方写。
pub fn restore() -> bool {
    let Some(owner) = LAST.lock().ok().and_then(|slot| *slot) else {
        return false;
    };
    imp::activate(owner)
}

#[cfg(target_os = "macos")]
mod imp {
    use super::Owner;
    use std::ffi::c_void;

    type Id = *mut c_void;
    type Sel = *const c_void;

    #[link(name = "AppKit", kind = "framework")]
    extern "C" {}

    extern "C" {
        fn objc_getClass(name: *const u8) -> Id;
        fn sel_registerName(name: *const u8) -> Sel;
        fn objc_msgSend();
    }

    /// objc_msgSend 是变参签名，Rust 这边必须按实际调用的形状转成函数指针再调，
    /// 否则 arm64 上参数传递方式对不上（浮点/整数寄存器分配不同），
    /// 表现是随机拿到垃圾值而不是干脆报错。
    unsafe fn msg_id(receiver: Id, sel: Sel) -> Id {
        let f: extern "C" fn(Id, Sel) -> Id = std::mem::transmute(objc_msgSend as *const ());
        f(receiver, sel)
    }

    unsafe fn msg_i32(receiver: Id, sel: Sel) -> i32 {
        let f: extern "C" fn(Id, Sel) -> i32 = std::mem::transmute(objc_msgSend as *const ());
        f(receiver, sel)
    }

    unsafe fn class(name: &[u8]) -> Id {
        objc_getClass(name.as_ptr())
    }

    unsafe fn sel(name: &[u8]) -> Sel {
        sel_registerName(name.as_ptr())
    }

    pub fn current() -> Option<Owner> {
        unsafe {
            let workspace = class(b"NSWorkspace\0");
            if workspace.is_null() {
                return None;
            }
            let shared = msg_id(workspace, sel(b"sharedWorkspace\0"));
            if shared.is_null() {
                return None;
            }
            let app = msg_id(shared, sel(b"frontmostApplication\0"));
            if app.is_null() {
                return None;
            }
            let pid = msg_i32(app, sel(b"processIdentifier\0"));
            // -1 是 NSRunningApplication 表示「已退出」的约定值
            if pid <= 0 {
                return None;
            }
            Some(Owner(pid as isize))
        }
    }

    pub fn activate(owner: Owner) -> bool {
        unsafe {
            let class = class(b"NSRunningApplication\0");
            if class.is_null() {
                return false;
            }

            let with_pid: extern "C" fn(Id, Sel, i32) -> Id =
                std::mem::transmute(objc_msgSend as *const ());
            let app = with_pid(
                class,
                sel(b"runningApplicationWithProcessIdentifier:\0"),
                owner.0 as i32,
            );
            // 程序在这期间被关掉了：拿不到实例，绝不能退回到「粘给当前前台」
            if app.is_null() {
                return false;
            }

            let activate: extern "C" fn(Id, Sel, u64) -> bool =
                std::mem::transmute(objc_msgSend as *const ());
            activate(app, sel(b"activateWithOptions:\0"), 0)
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::Owner;

    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> isize;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn IsWindow(hwnd: isize) -> i32;
    }

    pub fn current() -> Option<Owner> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd == 0 {
            return None;
        }
        Some(Owner(hwnd))
    }

    pub fn activate(owner: Owner) -> bool {
        unsafe {
            // 窗口可能在这期间被关掉了。不检查的话 SetForegroundWindow 会失败，
            // 但我们分不清「失败」和「切到了别的窗口」——那正是最危险的情况。
            if IsWindow(owner.0) == 0 {
                return false;
            }
            SetForegroundWindow(owner.0) != 0
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod imp {
    use super::Owner;

    pub fn current() -> Option<Owner> {
        // Linux 下没有跨桌面环境的通用做法（X11 和 Wayland 差别很大，
        // Wayland 甚至不允许程序知道谁在前台）。先不支持写回替换。
        None
    }

    pub fn activate(_owner: Owner) -> bool {
        false
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    /// objc 的消息发送是拿函数指针硬转出来的，签名写错不会编译失败，
    /// 只会在运行时拿到垃圾值——所以这块必须真跑一次才算数。
    /// 需要有图形会话（读的是当前前台程序），因此默认不跑：
    ///   cargo test --lib -- --ignored --nocapture reads_frontmost
    #[test]
    #[ignore = "需要图形会话"]
    fn reads_frontmost_application() {
        let owner = super::imp::current().expect("读不到前台程序");
        let pid = owner.0;
        println!("前台程序 pid = {pid}");

        assert!(pid > 0, "pid 必须是正数，拿到 {pid} 说明签名对不上");
        // 垃圾值几乎一定会超出合法的 pid 范围（macOS 上远小于这个数）
        assert!(pid < 1_000_000, "pid = {pid} 明显不是真实进程号");

        // 真的存在这个进程吗：kill(pid, 0) 只做权限与存在性检查，不发信号
        let alive = unsafe { libc_kill(pid as i32, 0) } == 0;
        assert!(alive, "pid {pid} 对应的进程不存在，说明读到的是垃圾值");
    }

    extern "C" {
        #[link_name = "kill"]
        fn libc_kill(pid: i32, sig: i32) -> i32;
    }
}
