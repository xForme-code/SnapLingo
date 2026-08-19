#!/usr/bin/env bash
# 构建 → 签名 → 生成更新清单 → 发布到 GitHub Release
#
# 用法: bash scripts/release-macos.sh [版本号]
#   版本号省略时用 tauri.conf.json 里的值。
#
# 两套签名不要混淆：
#   · 代码签名（SnapLingo Dev Signing）→ 让 macOS 允许运行
#   · 更新签名（~/.snaplingo-keys/updater.key）→ 让旧版本相信这个更新包是你发的
# 两者都必须有，缺一个用户就装不上或收不到更新。
set -euo pipefail

cd "$(dirname "$0")/.."

KEY="$HOME/.snaplingo-keys/updater.key"
CERT="SnapLingo Dev Signing"
REPO="xForme-code/snaplingo"

[[ -f "$KEY" ]] || { echo "[error] 找不到更新签名私钥: $KEY"; exit 1; }

VERSION="${1:-$(python3 -c 'import json;print(json.load(open("src-tauri/tauri.conf.json"))["version"])')}"
TAG="v$VERSION"
echo "[release] 版本 $TAG"

# ---------------------------------------------------------------- 构建
echo "[build] 编译 sidecar"
bash scripts/build-ocr-helper.sh
bash scripts/build-translate-helper.sh

echo "[build] 构建并签名（同时产出 DMG 和更新包）"
export TAURI_SIGNING_PRIVATE_KEY="$KEY"   # v2 认这个名字，值可以是路径或密钥内容
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""
export APPLE_SIGNING_IDENTITY="$CERT"
npx -y @tauri-apps/cli@^2 build --bundles dmg,app

BUNDLE="src-tauri/target/release/bundle"
# Tauri 的默认命名是 SnapLingo_0.2.1_aarch64.dmg，下面会统一改成
# snaplingo-<版本>.dmg。改名只在上传前做，构建产物本身不动。
DMG_RAW="$BUNDLE/dmg/SnapLingo_${VERSION}_aarch64.dmg"
DMG="$BUNDLE/dmg/snaplingo-${VERSION}.dmg"
TARBALL="$BUNDLE/macos/SnapLingo.app.tar.gz"
SIGFILE="$TARBALL.sig"

for f in "$DMG_RAW" "$TARBALL" "$SIGFILE"; do
  [[ -f "$f" ]] || { echo "[error] 缺少产物: $f"; exit 1; }
done

cp "$DMG_RAW" "$DMG"

# ---------------------------------------------------------------- 更新清单
# 文件名必须和上传到 Release 的一致，否则旧版本下载会 404。
# 这里和 DMG 用同一套命名，Release 页面看起来才整齐。
ASSET="snaplingo-${VERSION}.app.tar.gz"
cp "$TARBALL" "$BUNDLE/macos/$ASSET"

echo "[manifest] 生成 latest.json"
python3 - "$VERSION" "$SIGFILE" "$REPO" "$TAG" "$ASSET" > "$BUNDLE/latest.json" <<'PY'
import json, sys, datetime
version, sigfile, repo, tag, asset = sys.argv[1:6]
signature = open(sigfile).read().strip()
print(json.dumps({
    "version": version,
    "notes": f"SnapLingo {version}",
    "pub_date": datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z"),
    "platforms": {
        "darwin-aarch64": {
            "signature": signature,
            "url": f"https://github.com/{repo}/releases/download/{tag}/{asset}",
        }
    },
}, ensure_ascii=False, indent=2))
PY

# ---------------------------------------------------------------- 发布
echo "[publish] 上传到 GitHub Release $TAG"
# 发布说明：优先用 NOTES_FILE 指定的文件，没给就生成一份最简的。
# 不能缺省读 /dev/stdin —— 非交互环境下 gh 会一直挂着等输入。
if [[ -n "${NOTES_FILE:-}" && -f "${NOTES_FILE}" ]]; then
  NOTES_ARG=(--notes-file "$NOTES_FILE")
else
  NOTES_ARG=(--notes "SnapLingo $TAG")
fi

if gh release view "$TAG" >/dev/null 2>&1; then
  echo "[publish] Release 已存在，覆盖上传资产"
  gh release upload "$TAG" "$DMG" "$BUNDLE/macos/$ASSET" "$BUNDLE/latest.json" --clobber
else
  gh release create "$TAG" \
    "$DMG" "$BUNDLE/macos/$ASSET" "$BUNDLE/latest.json" \
    --title "SnapLingo $TAG" "${NOTES_ARG[@]}"
fi

echo ""
echo "[done] https://github.com/$REPO/releases/tag/$TAG"
echo "旧版本会从 releases/latest/download/latest.json 读到这次更新。"
