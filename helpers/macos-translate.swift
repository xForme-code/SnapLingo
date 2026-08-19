// SnapLingo macOS 翻译 helper
//
// 调用系统内置的 Translation 框架（macOS 15+）：端上推理、免费、离线、
// 不需要任何 API Key，也不受网络环境影响。编译产物很小，用完即退。
//
// 用法:
//   snaplingo-translate --check <from> <to>      查询语言对状态
//   snaplingo-translate --prepare <from> <to>    弹出系统下载界面（需要用户点确认）
//   snaplingo-translate <from> <to>              翻译，原文从 stdin 读
//
// 输出统一是 JSON: {"ok":true,"text":"...","status":"..."}
//
// ---------------------------------------------------------------- 实测得到的三个约束
//
// 1) Translation 的 async API 需要主线程 RunLoop 被泵动才会完成（内部走 XPC）。
//    用 DispatchSemaphore 阻塞主线程等结果会直接死等到超时。
//
// 2) TranslationSession 没有公开构造器，只能通过 SwiftUI 的 .translationTask
//    拿到。所以这里挂一个 SwiftUI 视图在屏幕外的透明窗口上——视图必须真正进入
//    渲染树，translationTask 才会触发。
//
// 3) 语言包没下载时，session.translate() 既不报错也不返回，会无限挂住。
//    因此翻译模式必须自带超时，并把超时解释成「需要下载语言包」。

import Foundation
import SwiftUI
import Translation

// ---------------------------------------------------------------- 输出

struct Output: Encodable {
    let ok: Bool
    var text: String = ""
    var status: String? = nil
    var error: String? = nil
}

func emit(_ output: Output) -> Never {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.withoutEscapingSlashes]
    if let data = try? encoder.encode(output), let json = String(data: data, encoding: .utf8) {
        print(json)
    } else {
        print(#"{"ok":false,"error":"encode failed"}"#)
    }
    exit(output.ok ? 0 : 1)
}

func failure(_ message: String, status: String? = nil) -> Never {
    emit(Output(ok: false, status: status, error: message))
}

/// 跨线程收集结果。translationTask 的闭包不在主线程上跑。
final class Sink: @unchecked Sendable {
    var done = false
    var text: String?
    var error: String?
}

/// 泵主线程 RunLoop 直到条件达成或超时（见文件头约束 1）
func pump(until sink: Sink, seconds: TimeInterval) {
    let deadline = Date().addingTimeInterval(seconds)
    while !sink.done, Date() < deadline {
        RunLoop.current.run(mode: .default, before: Date().addingTimeInterval(0.02))
    }
}

// ---------------------------------------------------------------- 可用性

func statusName(_ status: LanguageAvailability.Status) -> String {
    switch status {
    case .installed:   return "installed"
    case .supported:   return "supported"
    case .unsupported: return "unsupported"
    @unknown default:  return "unknown"
    }
}

func checkAvailability(from: String, to: String) -> Never {
    let sink = Sink()

    Task {
        let status = await LanguageAvailability().status(
            from: Locale.Language(identifier: from),
            to: Locale.Language(identifier: to)
        )
        sink.text = statusName(status)
        sink.done = true
    }

    pump(until: sink, seconds: 10)

    guard let status = sink.text else {
        failure("查询语言包状态超时")
    }
    emit(Output(ok: true, status: status))
}

// ---------------------------------------------------------------- 翻译 / 预备

/// 承载 translationTask 的最小视图（见文件头约束 2）
struct Worker: View {
    let config: TranslationSession.Configuration
    let text: String
    let prepareOnly: Bool
    let sink: Sink

    var body: some View {
        Color.clear
            .frame(width: 2, height: 2)
            .translationTask(config) { session in
                do {
                    if prepareOnly {
                        // 触发系统的语言包下载界面。窗口必须可见，否则弹不出来。
                        try await session.prepareTranslation()
                        sink.text = ""
                    } else {
                        sink.text = try await session.translate(text).targetText
                    }
                } catch {
                    sink.error = "\(error)"
                }
                sink.done = true
            }
    }
}

final class Delegate: NSObject, NSApplicationDelegate {
    let config: TranslationSession.Configuration
    let text: String
    let prepareOnly: Bool
    let sink: Sink
    let timeout: TimeInterval
    private var window: NSWindow?

    init(config: TranslationSession.Configuration, text: String, prepareOnly: Bool,
         sink: Sink, timeout: TimeInterval) {
        self.config = config
        self.text = text
        self.prepareOnly = prepareOnly
        self.sink = sink
        self.timeout = timeout
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        // 翻译时窗口要藏起来（用户不该看见任何东西）；
        // 下载语言包时必须可见，系统的下载确认界面才有地方显示。
        let size: CGFloat = prepareOnly ? 360 : 2
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: size, height: size * 0.6),
            styleMask: prepareOnly ? [.titled, .closable] : [.borderless],
            backing: .buffered,
            defer: false
        )
        window.title = "SnapLingo · 下载翻译语言包"
        window.alphaValue = prepareOnly ? 1 : 0
        window.ignoresMouseEvents = !prepareOnly
        window.level = prepareOnly ? .normal : .floating
        window.contentView = NSHostingView(
            rootView: Worker(config: config, text: text, prepareOnly: prepareOnly, sink: sink)
        )
        if prepareOnly {
            window.center()
            window.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
        } else {
            window.orderFrontRegardless()
        }
        self.window = window

        DispatchQueue.main.asyncAfter(deadline: .now() + timeout) { [weak self] in
            guard let self, !self.sink.done else { return }
            self.report()
        }

        // translationTask 的闭包在别的线程上跑完，这里轮询它的完成标志
        Timer.scheduledTimer(withTimeInterval: 0.05, repeats: true) { [weak self] timer in
            guard let self, self.sink.done else { return }
            timer.invalidate()
            self.report()
        }
    }

    private func report() -> Never {
        if let error = sink.error {
            failure(error)
        }
        guard let text = sink.text else {
            // 见文件头约束 3：语言包缺失时是挂住而不是报错，超时就是这个原因
            failure("语言包未就绪或系统翻译无响应", status: "needs-download")
        }
        emit(Output(ok: true, text: text, status: "installed"))
    }
}

func runSession(from: String, to: String, text: String, prepareOnly: Bool) -> Never {
    // source 传 nil 时系统自己识别源语言，比我们瞎猜准。
    // 但下载语言包必须指明具体语言对，所以那条路径要求调用方给出 from。
    let source: Locale.Language? =
        (from.isEmpty || from == "auto") ? nil : Locale.Language(identifier: from)

    let config = TranslationSession.Configuration(
        source: source,
        target: Locale.Language(identifier: to)
    )
    let app = NSApplication.shared
    let delegate = Delegate(
        config: config,
        text: text,
        prepareOnly: prepareOnly,
        sink: Sink(),
        // 下载要等用户操作，给足时间；翻译是端上推理，几百毫秒就该出来
        timeout: prepareOnly ? 300 : 12
    )
    app.delegate = delegate
    app.setActivationPolicy(prepareOnly ? .regular : .accessory)
    app.run()
    exit(0)
}

// ---------------------------------------------------------------- 入口

@main
struct Tool {
    static func main() {
        let args = CommandLine.arguments

        guard args.count >= 3 else {
            failure("usage: snaplingo-translate [--check|--prepare] <from> <to>")
        }

        switch args[1] {
        case "--check":
            guard args.count >= 4 else { failure("usage: --check <from> <to>") }
            checkAvailability(from: args[2], to: args[3])

        case "--prepare":
            guard args.count >= 4 else { failure("usage: --prepare <from> <to>") }
            runSession(from: args[2], to: args[3], text: "", prepareOnly: true)

        default:
            // 原文走 stdin：选中的文本可能很长，也可能含各种特殊字符，
            // 塞进命令行参数既有长度上限又容易出转义问题。
            let input = FileHandle.standardInput.readDataToEndOfFile()
            guard let text = String(data: input, encoding: .utf8), !text.isEmpty else {
                failure("stdin 没有可翻译的内容")
            }
            runSession(from: args[1], to: args[2], text: text, prepareOnly: false)
        }
    }
}
