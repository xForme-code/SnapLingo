// SnapLingo macOS OCR helper
//
// 调用系统内置的 Vision 框架做文字识别：免费、离线、零模型下载，
// 中文准确率远高于 tesseract。编译产物约 70KB，用完即退，不常驻内存。
//
// 用法: snaplingo-ocr <image-path> [lang1,lang2,...]
// 输出: JSON  {"ok":true,"text":"...","lines":[{"text":"..","confidence":0.99}]}

import Foundation
import CoreGraphics
import ImageIO
import Vision

struct Line: Encodable {
    let text: String
    let confidence: Float
}

struct Output: Encodable {
    let ok: Bool
    let text: String
    let lines: [Line]
    let error: String?
}

func emit(_ output: Output) -> Never {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.withoutEscapingSlashes]
    if let data = try? encoder.encode(output),
       let json = String(data: data, encoding: .utf8) {
        print(json)
    } else {
        print(#"{"ok":false,"text":"","lines":[],"error":"encode failed"}"#)
    }
    exit(output.ok ? 0 : 1)
}

func failure(_ message: String) -> Never {
    emit(Output(ok: false, text: "", lines: [], error: message))
}

// ---------------------------------------------------------------- 参数

let args = CommandLine.arguments
guard args.count >= 2 else {
    failure("usage: snaplingo-ocr <image-path> [languages]")
}

let imagePath = args[1]

// 语言顺序会影响识别倾向，把用户偏好的语言放前面
let languages: [String] = args.count >= 3 && !args[2].isEmpty
    ? args[2].split(separator: ",").map { String($0).trimmingCharacters(in: .whitespaces) }
    : ["zh-Hans", "en-US"]

// ---------------------------------------------------------------- 读图

guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: imagePath) as CFURL, nil),
      let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
    failure("cannot read image at \(imagePath)")
}

// ---------------------------------------------------------------- 识别

let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = true
request.recognitionLanguages = languages

do {
    try VNImageRequestHandler(cgImage: image, options: [:]).perform([request])
} catch {
    failure("vision failed: \(error.localizedDescription)")
}

let observations = request.results ?? []

// Vision 返回的顺序不保证是阅读顺序，按 y 从上到下、x 从左到右重排
let sorted = observations.sorted { a, b in
    let ay = a.boundingBox.midY
    let by = b.boundingBox.midY
    // boundingBox 原点在左下角，midY 越大越靠上
    if abs(ay - by) > 0.012 { return ay > by }
    return a.boundingBox.minX < b.boundingBox.minX
}

var lines: [Line] = []
for observation in sorted {
    guard let candidate = observation.topCandidates(1).first else { continue }
    lines.append(Line(text: candidate.string, confidence: candidate.confidence))
}

emit(Output(ok: true,
            text: lines.map(\.text).joined(separator: "\n"),
            lines: lines,
            error: nil))
