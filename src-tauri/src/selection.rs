use anyhow::{anyhow, Result};
use arboard::Clipboard;
#[cfg(not(target_os = "macos"))]
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::thread;
use std::time::{Duration, Instant};

/// 一个正常内容不可能撞上的哨兵串。
/// 先把它写进剪贴板，再模拟复制；只要剪贴板变了，就说明复制真的发生了。
/// 这比「比较前后文本是否相同」可靠——用户完全可能复制一段和剪贴板里一样的文字。
const SENTINEL: &str = "\u{200B}__snaplingo_sentinel__\u{200B}";

/// 划词是被动触发的，第一条通道的超时要短——
/// 正常情况下 ⌘C 在 50~150ms 内就会让剪贴板变化，这里等不到
/// 就应该尽快切到兜底通道，而不是原地耗满 700ms。
const FAST_TIMEOUT: Duration = Duration::from_millis(260);

/// 快捷键是用户主动按的，他在等结果，这时候宁可慢也要成
const THOROUGH_TIMEOUT: Duration = Duration::from_millis(700);

/// 划词场合走兜底通道时的等待上限。
/// 兜底本身要起子进程（约 150~300ms），这里不必再给满 700ms，
/// 只要够目标 App 完成一次复制即可。
#[cfg(target_os = "macos")]
const FAST_FALLBACK_TIMEOUT: Duration = Duration::from_millis(500);

const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// 取词的两种场合，决定超时长短和是否启用兜底通道
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    /// 划词触发：抢时间，失败就算了
    Fast,
    /// 快捷键触发：抢成功率，允许多花时间走兜底
    Thorough,
}

#[derive(Debug, Clone)]
pub(crate) enum Snapshot {
    Text(String),
    Image {
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    },
    Empty,
}

pub(crate) fn snapshot(clipboard: &mut Clipboard) -> Snapshot {
    if let Ok(text) = clipboard.get_text() {
        if !text.is_empty() {
            return Snapshot::Text(text);
        }
    }
    if let Ok(img) = clipboard.get_image() {
        return Snapshot::Image {
            width: img.width,
            height: img.height,
            bytes: img.bytes.into_owned(),
        };
    }
    Snapshot::Empty
}

pub(crate) fn restore(clipboard: &mut Clipboard, snap: Snapshot) {
    let result = match snap {
        Snapshot::Text(text) => clipboard.set_text(text),
        Snapshot::Image {
            width,
            height,
            bytes,
        } => clipboard.set_image(arboard::ImageData {
            width,
            height,
            bytes: bytes.into(),
        }),
        Snapshot::Empty => clipboard.clear(),
    };
    if let Err(err) = result {
        log::warn!("还原剪贴板失败: {err}");
    }
}

/// macOS 上直接用 CoreGraphics 合成 ⌘C。
///
/// 不用 enigo 的原因：它把 ⌘ 和 C 作为两个独立事件投递，而大量 App 是读
/// `CGEventGetFlags()` 来判断修饰键的——C 键事件本身不带 maskCommand 标志时，
/// App 只会看到一个普通的字符 c，根本不会触发复制。实测在 PDF 阅读器和终端里
/// enigo 报告发送成功，但剪贴板始终没有变化，就是这个原因。
///
/// 这里把标志位直接设在按键事件上，等价于用户真按了 ⌘C。
#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::c_void;

    type CFTypeRef = *mut c_void;

    const KEY_C: u16 = 8; // kVK_ANSI_C
    const KEY_V: u16 = 9; // kVK_ANSI_V
    const KEY_COMMAND: u16 = 55; // kVK_Command
    const FLAG_COMMAND: u64 = 0x0010_0000; // kCGEventFlagMaskCommand
    const HID_EVENT_TAP: u32 = 0; // kCGHIDEventTap
    const SOURCE_HID_SYSTEM_STATE: i32 = 1; // kCGEventSourceStateHIDSystemState

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventSourceCreate(state_id: i32) -> CFTypeRef;
        fn CGEventCreateKeyboardEvent(source: CFTypeRef, keycode: u16, key_down: bool) -> CFTypeRef;
        fn CGEventSetFlags(event: CFTypeRef, flags: u64);
        fn CGEventPost(tap: u32, event: CFTypeRef);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(cf: CFTypeRef);
    }

    /// 释放 CF 对象的 RAII 包装，避免任何一条错误路径漏掉 CFRelease
    struct Owned(CFTypeRef);

    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) };
            }
        }
    }

    pub fn send_command_c() -> Result<(), String> {
        send_command_key(KEY_C)
    }

    /// 模拟 ⌘V。写回替换用——键码之外的一切和 ⌘C 完全一样。
    pub fn send_command_v() -> Result<(), String> {
        send_command_key(KEY_V)
    }

    fn send_command_key(key: u16) -> Result<(), String> {
        unsafe {
            let source = Owned(CGEventSourceCreate(SOURCE_HID_SYSTEM_STATE));
            // source 为空也能继续（系统会用默认源），不算致命错误

            // 完整还原真实按键序列：⌘ 按下 → C 按下 → C 抬起 → ⌘ 抬起。
            //
            // 不能只发 C 键事件再贴上 Command 标志位：跟踪 flagsChanged 的
            // App（Electron、Java、虚拟机等）看不到修饰键的按下过程，
            // 会把它当成一个普通的 c 而不触发复制。
            let cmd_down = Owned(CGEventCreateKeyboardEvent(source.0, KEY_COMMAND, true));
            let c_down = Owned(CGEventCreateKeyboardEvent(source.0, key, true));
            let c_up = Owned(CGEventCreateKeyboardEvent(source.0, key, false));
            let cmd_up = Owned(CGEventCreateKeyboardEvent(source.0, KEY_COMMAND, false));
            if cmd_down.0.is_null() || c_down.0.is_null() || c_up.0.is_null() || cmd_up.0.is_null()
            {
                return Err("创建键盘事件失败".into());
            }

            // C 键事件仍要自带 Command 标志：读 CGEventGetFlags 的 App 只看这里
            CGEventSetFlags(cmd_down.0, FLAG_COMMAND);
            CGEventSetFlags(c_down.0, FLAG_COMMAND);
            CGEventSetFlags(c_up.0, FLAG_COMMAND);

            let gap = std::time::Duration::from_millis(8);
            CGEventPost(HID_EVENT_TAP, cmd_down.0);
            std::thread::sleep(gap);
            CGEventPost(HID_EVENT_TAP, c_down.0);
            std::thread::sleep(gap);
            CGEventPost(HID_EVENT_TAP, c_up.0);
            std::thread::sleep(gap);
            CGEventPost(HID_EVENT_TAP, cmd_up.0);
        }
        Ok(())
    }
}

/// 非 macOS 平台仍走 enigo，用物理键码绕开键盘布局反查
#[cfg(target_os = "windows")]
const COPY_KEY: Key = Key::Other(0x43); // VK_C
#[cfg(target_os = "windows")]
const PASTE_KEY: Key = Key::Other(0x56); // VK_V
#[cfg(target_os = "linux")]
const COPY_KEY: Key = Key::Other(54); // X11 keycode for 'c'
#[cfg(target_os = "linux")]
const PASTE_KEY: Key = Key::Other(55); // X11 keycode for 'v'

#[cfg(target_os = "macos")]
fn send_copy() -> Result<()> {
    mac::send_command_c().map_err(|e| anyhow!("{e}（macOS 需要辅助功能权限）"))
}

#[cfg(not(target_os = "macos"))]
fn send_copy() -> Result<()> {
    send_with_ctrl(COPY_KEY)
}

/// 模拟 Ctrl+<键>。复制和粘贴只差一个键码，其余讲究完全一样。
#[cfg(not(target_os = "macos"))]
fn send_with_ctrl(key: Key) -> Result<()> {
    let mut enigo = Enigo::new(&Settings::default())
        .map_err(|e| anyhow!("初始化输入模拟失败: {e}"))?;

    enigo
        .key(Key::Control, Direction::Press)
        .map_err(|e| anyhow!("按下修饰键失败: {e}"))?;

    // 修饰键按下后给系统一点时间登记，否则字母键会被当成普通字符输入
    thread::sleep(Duration::from_millis(20));

    let click = enigo.key(key, Direction::Click);
    // 无论成没成，修饰键都必须松开，否则会卡住用户的键盘
    let release = enigo.key(Key::Control, Direction::Release);

    click.map_err(|e| anyhow!("发送按键失败: {e}"))?;
    release.map_err(|e| anyhow!("释放修饰键失败: {e}"))?;
    Ok(())
}

/// 模拟「粘贴」。写回替换用。
#[cfg(target_os = "macos")]
pub(crate) fn send_paste() -> Result<()> {
    mac::send_command_v().map_err(|e| anyhow!("{e}（macOS 需要辅助功能权限）"))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn send_paste() -> Result<()> {
    send_with_ctrl(PASTE_KEY)
}

/// 通过 Apple Events 发 ⌘C 的兜底通道。
///
/// 比 CGEvent 慢（要起子进程），但走的是完全不同的系统路径，
/// 某些 App 对合成的 HID 事件不响应时，这条反而能成。
#[cfg(target_os = "macos")]
fn send_copy_via_osascript() -> Result<()> {
    let status = std::process::Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to keystroke \"c\" using command down")
        .status()
        .map_err(|e| anyhow!("调用 osascript 失败: {e}"))?;
    if !status.success() {
        return Err(anyhow!("osascript 发送 ⌘C 失败（可能缺少辅助功能权限）"));
    }
    Ok(())
}

/// 发一次复制并等待剪贴板变化。返回取到的文本。
fn attempt_copy(
    clipboard: &mut Clipboard,
    channel: &str,
    timeout: Duration,
    send: impl FnOnce() -> Result<()>,
) -> Option<String> {
    let started = Instant::now();

    if let Err(err) = send() {
        log::warn!("[{channel}] 发送复制快捷键失败: {err}");
        return None;
    }

    let deadline = started + timeout;
    while Instant::now() < deadline {
        if let Ok(current) = clipboard.get_text() {
            if current != SENTINEL {
                log::debug!(
                    "[{channel}] 剪贴板在 {}ms 后变化，取到 {} 字",
                    started.elapsed().as_millis(),
                    current.chars().count()
                );
                return Some(current);
            }
        }
        thread::sleep(POLL_INTERVAL);
    }

    log::warn!(
        "[{channel}] 等待 {}ms 后剪贴板仍是哨兵值",
        started.elapsed().as_millis()
    );
    None
}

/// 取当前选中的文字。
///
/// 流程：备份剪贴板 → 写哨兵 → 模拟复制 → 轮询到剪贴板变化 → 还原剪贴板。
/// 对用户的剪贴板完全无副作用。
pub fn capture_selected_text() -> Result<Option<String>> {
    capture_with_mode(CaptureMode::Thorough)
}

pub fn capture_with_mode(mode: CaptureMode) -> Result<Option<String>> {
    // 必须在这里记：此刻气泡还没弹出来，焦点还在用户真正在用的那个程序上。
    // 等用户点了气泡再问「谁是前台」，答案就变成 SnapLingo 自己了。
    crate::frontmost::remember();

    let mut clipboard = Clipboard::new().map_err(|e| anyhow!("无法访问剪贴板: {e}"))?;
    let backup = snapshot(&mut clipboard);

    clipboard
        .set_text(SENTINEL)
        .map_err(|e| anyhow!("写入剪贴板失败: {e}"))?;

    let timeout = match mode {
        CaptureMode::Fast => FAST_TIMEOUT,
        CaptureMode::Thorough => THOROUGH_TIMEOUT,
    };

    // 先走原生事件合成；失败了再用 osascript 兜底。
    // 两条通道机制完全不同（CoreGraphics 事件 vs Apple Events），
    // 某个 App 挡住其中一条时，另一条往往还能过去。
    let mut captured = attempt_copy(&mut clipboard, "CGEvent", timeout, send_copy);

    // 兜底通道要起子进程，慢一些，但划词场合也必须走：
    // 有些系统/App 组合下 CGEvent 通道会整体失效，只砍掉兜底的话
    // 划词就永远弹不出气泡。气泡位置早已锚定在手势坐标上，
    // 晚几百毫秒弹出仍然贴在选区旁边，比完全不弹好得多。
    #[cfg(target_os = "macos")]
    if captured.is_none() {
        let fallback_timeout = match mode {
            CaptureMode::Fast => FAST_FALLBACK_TIMEOUT,
            CaptureMode::Thorough => THOROUGH_TIMEOUT,
        };
        captured = attempt_copy(
            &mut clipboard,
            "osascript",
            fallback_timeout,
            send_copy_via_osascript,
        );
    }

    if captured.is_none() && mode == CaptureMode::Thorough {
        // 只在全部失败后才查前台 App：这一步要起子进程，不能放在正常路径上。
        // 它能区分两种截然不同的失败——按键根本没送达，还是送给了错误的 App。
        log::warn!(
            "两条通道都没能取到内容；当时前台 App = {}",
            frontmost_app().unwrap_or_else(|| "查询失败".into())
        );
    }

    // 稍等一下再还原，避免和目标 App 的剪贴板写入抢跑
    thread::sleep(Duration::from_millis(40));
    restore(&mut clipboard, backup);

    // 发送失败已经在 attempt_copy 里记过日志了。这里不再向上抛错——
    // 取不到词是常态（用户可能确实没选中东西），不该当成异常打断流程。
    Ok(captured
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty()))
}

/// 查询当前前台 App 名称，仅用于排查失败原因。
///
/// 走 osascript 而不是引入 objc2-app-kit：这条路径只在取词失败时执行，
/// 多花一百毫秒换少一个依赖是划算的。
#[cfg(target_os = "macos")]
fn frontmost_app() -> Option<String> {
    let output = std::process::Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to name of first application process whose frontmost is true")
        .output()
        .ok()?;
    let name = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(not(target_os = "macos"))]
fn frontmost_app() -> Option<String> {
    None
}

/// 翻译前的文本预处理。
///
/// 把 `user_name` / `getUserInfo` / `get-user-info` 这类标识符拆成自然语言，
/// 否则翻译引擎会把整个标识符当成一个不认识的词直接吐回来。
pub fn split_identifiers(text: &str) -> String {
    // 只在整段看起来像单个标识符时才处理，避免破坏正常句子
    let looks_like_identifier = !text.contains(' ')
        && text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');

    if !looks_like_identifier {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len() + 8);
    let chars: Vec<char> = text.chars().collect();
    for (i, &ch) in chars.iter().enumerate() {
        match ch {
            '_' | '-' | '.' => out.push(' '),
            c if c.is_ascii_uppercase() => {
                // camelCase → camel Case，但连续大写（如 HTTPServer）只在末尾断开
                let prev_lower = i > 0 && chars[i - 1].is_ascii_lowercase();
                let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
                if i > 0 && (prev_lower || (chars[i - 1].is_ascii_uppercase() && next_lower)) {
                    out.push(' ');
                }
                out.push(c.to_ascii_lowercase());
            }
            c => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::{capture_selected_text, split_identifiers};

    /// 端到端验证取词：用 TextEdit 造一个真实选区，再走我们自己的取词流程。
    ///
    /// 需要终端具备辅助功能权限，且会短暂打开 TextEdit，所以默认不跑：
    ///   cargo test --lib -- --ignored --nocapture real_copy
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn real_copy_from_textedit() {
        use std::process::Command;

        const PROBE: &str = "SnapLingo probe 深空笔记 12345";

        let script = format!(
            r#"
            tell application "TextEdit"
                activate
                make new document with properties {{text:"{PROBE}"}}
            end tell
            delay 1.2
            tell application "System Events" to keystroke "a" using command down
            delay 0.4
            "#
        );
        Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .status()
            .expect("准备 TextEdit 选区失败");

        let captured = capture_selected_text().expect("取词调用本身出错");

        // 无论成败都要收拾干净，别给用户留一个未保存的窗口
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(r#"tell application "TextEdit" to close every document saving no"#)
            .status();

        println!("取到: {captured:?}");
        assert_eq!(
            captured.as_deref().map(str::trim),
            Some(PROBE),
            "取词内容与选区不一致——模拟复制没有真正生效"
        );
    }

    #[test]
    fn splits_common_identifier_styles() {
        assert_eq!(split_identifiers("user_name"), "user name");
        assert_eq!(split_identifiers("getUserInfo"), "get user info");
        assert_eq!(split_identifiers("get-user-info"), "get user info");
        assert_eq!(split_identifiers("HTTPServerError"), "http server error");
    }

    #[test]
    fn leaves_normal_sentences_alone() {
        let sentence = "The quick brown fox.";
        assert_eq!(split_identifiers(sentence), sentence);
    }
}
