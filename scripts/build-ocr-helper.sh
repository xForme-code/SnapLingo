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

swiftc -O helpers/macos-ocr.swift -o "$OUT"
chmod +x "$OUT"

echo "[ocr-helper] 已生成 $OUT ($(du -h "$OUT" | cut -f1))"
