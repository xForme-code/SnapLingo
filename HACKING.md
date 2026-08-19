# 改代码指南

给想动手改这个项目的人。按「你想改什么」组织，不是按文件组织。

---

## 先跑起来

```bash
brew install cmake               # 只需一次，编译 CTranslate2（离线翻译引擎）要用
cd ~/Desktop/snaplingo
npm install                      # 只需一次
bash scripts/install-macos.sh    # 构建 + 固定签名 + 装到 ~/Applications
open ~/Applications/SnapLingo.app
```

**cmake 是硬性前提**：`ct2rs`（OPUS-MT 离线引擎）会从源码编译 CTranslate2，
没有 cmake 直接构建失败。首次编译这一项约 1 分钟，之后走缓存。

改完 Rust 代码后重复最后两步。首次编译 3~5 分钟，之后增量编译约 30 秒。

**不要用 `codesign -s -` 重新签名**。`install-macos.sh` 会用钥匙串里那张固定的自签证书
（`SnapLingo Dev Signing`），这样 macOS 的辅助功能授权才能跨重建保留。换成 ad-hoc 签名的话，
每次编译授权都会作废，你会被反复要求重新授权。

## 排查的第一现场

```bash
tail -f ~/Library/Application\ Support/SnapLingo/snaplingo.log
```

日志每次启动清空。debug 构建记 DEBUG 级别，划词的每一步都有记录：鼠标按下/抬起的坐标、
拖动距离、判定结果、取词用了哪条通道、耗时多少、取到几个字。

配置文件在 `~/Library/Application Support/SnapLingo/config.json`，改完重启生效。

---

## 想改什么，看哪里

### 划词取不到内容（当前最主要的问题）

整条链路按顺序是四步，每步都可以单独下手：

| 步骤 | 文件 | 关键函数 |
|---|---|---|
| 1. 监听鼠标，判定是不是划选 | `src-tauri/src/hooks.rs` | `start_mouse_watcher` 里的 `ButtonRelease` 分支 |
| 2. 模拟复制键 | `src-tauri/src/selection.rs` | `send_copy` / `mac::send_command_c` |
| 3. 轮询剪贴板拿结果 | `src-tauri/src/selection.rs` | `attempt_copy` |
| 4. 弹气泡 | `src-tauri/src/windows.rs` | `show_bubble` |

**第 2 步是目前最可疑的地方。** 现在有两条通道：

```rust
// selection.rs — 先原生事件，失败再 osascript 兜底
let mut captured = attempt_copy(&mut clipboard, "CGEvent", timeout, send_copy);
if captured.is_none() && mode == CaptureMode::Thorough {
    captured = attempt_copy(&mut clipboard, "osascript", THOROUGH_TIMEOUT, send_copy_via_osascript);
}
```

`mac::send_command_c()` 直接调 CoreGraphics 合成 ⌘C，关键是把 `maskCommand`
标志位设在按键事件本身上（而不是靠单独发一个 ⌘ 按下事件）。如果某个 App 还是不响应，
可以试的方向：

- 把 `CGEventPost` 的目标从 `kCGHIDEventTap`(0) 换成 `kCGSessionEventTap`(1)
- 在 keyDown 之前先发一个 `flagsChanged` 事件
- 加大 keyDown 和 keyUp 之间的间隔（现在 12ms）
- 用 `CGEventPostToPid` 直接投给前台进程

日志会告诉你走到哪一步失败的，失败时还会记录当时的前台 App 名称。

### 触发太灵敏 / 不灵敏

`src-tauri/src/hooks.rs` 的 `ButtonRelease` 分支。两个判定条件：

```rust
let dragged = distance >= cfg.drag_threshold;          // 默认 6 像素，设置里可调
let double_clicked = ... < Duration::from_millis(400)  // 双击间隔
                     && ...distance < 6.0;             // 双击位移容差
```

后两个数字写死在代码里，想调得改这里。

### 气泡弹得太慢 / 位置不对

延时来自 `src-tauri/src/selection.rs` 顶部的两个常量：

```rust
const FAST_TIMEOUT: Duration = Duration::from_millis(260);      // 划词触发
const THOROUGH_TIMEOUT: Duration = Duration::from_millis(700);  // 快捷键触发
```

外加 `lib.rs` 里 `start_selection_pipeline` 中等待选区更新的 90ms。

位置逻辑在 `windows.rs` 的 `show_bubble`，`anchor` 是手势发生时记下的坐标（逻辑点）。

### 加一个翻译引擎

1. 在 `src-tauri/src/translate/` 下照着 `google.rs` 写一个新文件，导出
   `pub async fn translate(text: &str, target: &str) -> Result<Translation>`
2. `translate/mod.rs` 里三处登记：`pub mod` 声明、`list_providers()` 加一项、
   `translate()` 的 `match` 加一个分支
3. 前端不用改，引擎列表是从后端读的

### 改内容提取的规则

`src-tauri/src/extract.rs` 顶部的 `PATTERNS` 数组，一行一条正则。
底下有单元测试，加规则时顺手加个断言：

```bash
cd src-tauri && cargo test --lib
```

### 改界面

前端是纯 HTML/CSS/JS，**没有打包工具链**，改完直接重新构建就能看到。

| 界面 | 文件 |
|---|---|
| 划词图标条 | `src/bubble.html` / `bubble.js` |
| 翻译结果面板 | `src/result.html` / `result.js` |
| 收集夹 | `src/collector.html` / `collector.js` |
| 设置 | `src/settings.html` / `settings.js` |
| 配色和通用控件 | `src/common.css` |

所有颜色都走 CSS 变量，明暗主题在 `common.css` 顶部两个 `:root` 块里定义。

前后端通信只有一个入口：`src/api.js`。它把 Rust 的
`#[tauri::command]` 包装成普通 async 函数。新增命令要改两处——
`src-tauri/src/commands.rs` 写函数，`lib.rs` 的 `generate_handler!` 里登记。

### 改 OCR

`src-tauri/src/ocr.rs` 按平台分发。macOS 走 sidecar：

```
helpers/macos-ocr.swift          →  scripts/build-ocr-helper.sh 编译成
src-tauri/binaries/snaplingo-ocr-<target-triple>
```

改 Swift 后要重新跑 `bash scripts/build-ocr-helper.sh`（`install-macos.sh` 会自动调用）。

`ocr.rs` 底部的 `normalize()` 负责清洗识别结果（去掉 CJK 之间被误插的空格等），有单元测试。

---

## 几个容易踩的坑

**不能在 `setup()` 里创建窗口。** macOS 的应用生命周期还没走完，WKWebView 的内容进程会被立刻终止。
要推迟到 `RunEvent::Ready`（`lib.rs` 里 `PENDING_FIRST_RUN` 就是干这个的）。

**`RunEvent::ExitRequested` 不能无条件拦截。** 只能拦 `code.is_none()` 的那次（窗口关完触发的），
菜单里 `app.exit(0)` 发起的 `code` 是 `Some(0)`，拦了程序就永远关不掉。

**rdev 在 macOS 上不推送拖拽期间的移动事件。** 系统发的是 `LeftMouseDragged`，rdev 不转成
`MouseMove`，所以不能靠累积 `MouseMove` 来跟踪光标——必须在按下和抬起时各查询一次系统光标位置
（`hooks.rs` 的 `query_cursor()`）。

**rdev 的回调是阻塞的。** 任何耗时操作都必须挪到别的线程，否则会拖慢整个系统的鼠标事件派发。
现在的做法是回调里只发一个消息到 channel，实际取词在 `snaplingo-selection` 线程里做。

**日志时间戳要用本地时间。** `logging.rs` 里读了系统时区偏移。之前直接用 UTC 小时数，
排查时日志时间和实际差 8 小时，会误判成程序卡死。

**bash 脚本里全角括号紧贴变量名会出事。** `"...（$VAR）"` 里的 `）` 是多字节字符，
bash 会把它当成变量名的一部分。要写成 `${VAR}`。

---

## 测试

```bash
cd src-tauri
cargo test --lib                            # 7 项单元测试，不依赖系统交互
cargo test --lib -- --ignored --nocapture   # 含真实网络请求和系统交互的测试
```

`--ignored` 那批里有个 `real_copy_from_textedit`，它会打开 TextEdit 造一个真实选区再走取词流程。
**注意：它要求运行测试的终端本身有辅助功能权限**，否则测的是环境不是代码——
可以先用纯 osascript 做对照来确认环境是否成立：

```bash
osascript -e 'tell application "System Events" to keystroke "c" using command down'
```

如果这条也不生效，说明终端没有权限，测试结果不能采信。

---

## 当前状态

**能用**：编译、单元测试、Google 翻译、macOS Vision OCR、托盘、快捷键注册、鼠标钩子收事件、
内容提取、收集夹、设置界面。

**没确认**：拖选取词在终端和 PDF 阅读器里能否成功。这是唯一的阻塞项。

**没做**：Windows / Linux 的截图框选遮罩；这两个平台整体没在真机验证过。
