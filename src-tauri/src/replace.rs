//! 把译文写回用户正在编辑的文档，替换掉原来选中的那段。
//!
//! 这是整个程序里**唯一会改动用户文档**的功能，所以规则定得比别处严：
//! 任何一步不确定，就完全不写回，退化成「译文已放进剪贴板，你自己粘」。
//! 最坏情况等于现在的手工流程，而不是把文件改坏。

use anyhow::{anyhow, Result};
use arboard::Clipboard;
use std::time::Duration;

use crate::selection;

/// 写回失败但译文已经在剪贴板里时用这个前缀，前端据此提示「可手动粘贴」。
pub const IN_CLIPBOARD: &str = "IN_CLIPBOARD";

/// 切回前台程序后等多久再粘贴。
///
/// 激活是异步的：函数返回时系统只是受理了请求，窗口真正拿到键盘焦点还要几帧。
/// 等不够就会把 ⌘V 发给还没让位的那个程序（多半是我们自己），表现成「点了没反应」。
const ACTIVATE_SETTLE: Duration = Duration::from_millis(180);

/// 粘贴之后、还原剪贴板之前等多久。
///
/// 目标程序读剪贴板同样不是瞬时的。还原太早，用户文档里粘进去的就成了
/// **他上一次复制的内容**——比不还原糟糕得多。宁可多等。
const PASTE_SETTLE: Duration = Duration::from_millis(600);

/// 用 `replacement` 替换掉用户当前选中的文字。
///
/// 调用前提：选区还在、并且 `frontmost::remember()` 已经记下了源程序。
pub fn write_back(replacement: &str) -> Result<()> {
    if replacement.trim().is_empty() {
        return Err(anyhow!("译文是空的，不做替换"));
    }

    let mut clipboard = Clipboard::new().map_err(|e| anyhow!("无法访问剪贴板: {e}"))?;
    let backup = selection::snapshot(&mut clipboard);

    clipboard
        .set_text(replacement)
        .map_err(|e| anyhow!("写入剪贴板失败: {e}"))?;

    // 焦点切不回去就到此为止。硬粘贴的话，译文会进到「当前恰好在前台的那个
    // 程序」——可能是我们自己的窗口，也可能是用户刚才碰过的另一个文档。
    // 往错误的文件里写东西，比什么都不做严重得多。
    if !crate::frontmost::restore() {
        log::warn!("切不回原程序，放弃写回，译文留在剪贴板");
        return Err(anyhow!(
            "{IN_CLIPBOARD}切不回原来的程序，译文已复制到剪贴板，请手动粘贴"
        ));
    }

    std::thread::sleep(ACTIVATE_SETTLE);

    if let Err(err) = selection::send_paste() {
        log::warn!("模拟粘贴失败: {err}");
        return Err(anyhow!("{IN_CLIPBOARD}译文已复制到剪贴板，请手动粘贴（{err}）"));
    }

    std::thread::sleep(PASTE_SETTLE);

    // 只在剪贴板里还是我们放的那份译文时才还原。用户在这几百毫秒里自己复制了
    // 别的东西的话，还原就等于把他刚复制的内容悄悄换掉。
    let ours = clipboard.get_text().map(|t| t == replacement).unwrap_or(false);
    if ours {
        selection::restore(&mut clipboard, backup);
    } else {
        log::debug!("剪贴板已被别处改动，不还原");
    }

    Ok(())
}
