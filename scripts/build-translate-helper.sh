#!/usr/bin/env bash
# 编译 macOS 翻译 sidecar。Tauri 要求 sidecar 文件名带 target triple 后缀。
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "[translate-helper] 非 macOS，跳过（其它平台走 OPUS-MT 或云端引擎）"
  exit 0
fi

# Translation 框架要 macOS 15+。低版本上编译不出来，此时跳过：
# Rust 侧发现没有 sidecar 会自动回落到其它引擎。
MAJOR="$(sw_vers -productVersion | cut -d. -f1)"
if [[ "$MAJOR" -lt 15 ]]; then
  echo "[translate-helper] 当前 macOS $MAJOR 低于 15，跳过系统翻译组件"
  exit 0
fi

TRIPLE="$(rustc -vV | awk '/^host:/ {print $2}')"
OUT="src-tauri/binaries/snaplingo-translate-${TRIPLE}"

mkdir -p src-tauri/binaries

# -parse-as-library 配合 @main：否则 Swift 会把顶层代码当脚本处理，和 @main 冲突
# 同 OCR helper：必须显式指定部署目标，否则 minos 会变成编译机器的系统版本，
# 在更低版本的 Mac 上无法启动。Translation 框架从 macOS 15 才有，所以定 15.0——
# 低于 15 的系统上这个 sidecar 启动失败，Rust 侧会回落到 OPUS-MT 或云端。
swiftc -O -parse-as-library -target arm64-apple-macos15.0 helpers/macos-translate.swift -o "$OUT"
chmod +x "$OUT"

echo "[translate-helper] 已生成 $OUT ($(du -h "$OUT" | cut -f1))"
