#!/usr/bin/env bash
# 编译 macOS OCR sidecar。Tauri 要求 sidecar 文件名带 target triple 后缀。
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "[ocr-helper] 非 macOS，跳过（Windows 用系统 OCR API，Linux 用 ONNX 模型）"
  exit 0
fi

TRIPLE="$(rustc -vV | awk '/^host:/ {print $2}')"
OUT="src-tauri/binaries/snaplingo-ocr-${TRIPLE}"

mkdir -p src-tauri/binaries

# 必须显式指定部署目标！swiftc 默认用**编译机器**的系统版本，
# 在 macOS 26 上编出来的产物 minos 就是 26.0，装到别人 macOS 14/15 的
# 机器上会直接拒绝启动（dyld: app requires macOS 26.0 or later），
# 表现为截图取词毫无反应。Vision 框架 macOS 10.15 就有，这里对齐 App 的 11.0。
swiftc -O -target arm64-apple-macos11.0 helpers/macos-ocr.swift -o "$OUT"
chmod +x "$OUT"

echo "[ocr-helper] 已生成 $OUT ($(du -h "$OUT" | cut -f1))"
