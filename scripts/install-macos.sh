#!/usr/bin/env bash
# 构建 → 签名 → 安装到 ~/Applications
#
# 为什么要用固定的自签名证书而不是 ad-hoc：
#   ad-hoc 签名的 designated requirement 是 cdhash，每次重新编译都会变，
#   macOS 的 TCC 就认为这是另一个程序，之前授予的「辅助功能」权限全部作废。
#   用一张固定证书后，DR 变成「identifier + certificate leaf」，两者都不随
#   编译改变，授权因此可以跨重建保留。
set -euo pipefail

cd "$(dirname "$0")/.."

CERT_NAME="SnapLingo Dev Signing"
APP_NAME="SnapLingo.app"
BUILD_MODE="${1:-debug}"

# ---------------------------------------------------------------- 证书

ensure_cert() {
  if security find-identity -p codesigning 2>/dev/null | grep -q "$CERT_NAME"; then
    return 0
  fi

  echo "[sign] 首次运行，创建固定签名证书「$CERT_NAME」"
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  cat > "$tmp/cfg" <<EOF
[req]
distinguished_name = dn
x509_extensions = v3
prompt = no
[dn]
CN = $CERT_NAME
[v3]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF

  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
    -keyout "$tmp/k.pem" -out "$tmp/c.pem" -config "$tmp/cfg" 2>/dev/null

  # macOS 的 Security 框架只认老式 PKCS12 编码，OpenSSL 3 的默认算法会导入失败
  openssl pkcs12 -export -inkey "$tmp/k.pem" -in "$tmp/c.pem" -out "$tmp/c.p12" \
    -passout pass:snaplingo -name "$CERT_NAME" \
    -macalg sha1 -certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES 2>/dev/null

  # -A 让 codesign 使用私钥时不再弹授权框
  security import "$tmp/c.p12" -k ~/Library/Keychains/login.keychain-db \
    -P snaplingo -T /usr/bin/codesign -A
}

# ---------------------------------------------------------------- 构建

echo "[build] 编译 OCR helper"
bash scripts/build-ocr-helper.sh

# 变量名要用花括号界定：全角括号是多字节字符，紧贴 $VAR 时会被当成变量名的一部分
echo "[build] 构建应用（${BUILD_MODE}）"
if [[ "$BUILD_MODE" == "release" ]]; then
  npx tauri build --bundles app
  BUNDLE_DIR="src-tauri/target/release/bundle/macos"
else
  npx tauri build --debug --bundles app
  BUNDLE_DIR="src-tauri/target/debug/bundle/macos"
fi

SRC="$BUNDLE_DIR/$APP_NAME"
[[ -d "$SRC" ]] || { echo "[error] 没有找到构建产物 $SRC"; exit 1; }

# ---------------------------------------------------------------- 签名

ensure_cert
IDENTITY="$CERT_NAME"
if ! security find-identity -p codesigning 2>/dev/null | grep -q "$CERT_NAME"; then
  echo "[sign] 警告：证书不可用，回退到 ad-hoc（重建后需要重新授权）"
  IDENTITY="-"
fi

echo "[sign] 使用身份: $IDENTITY"
# 先签内部的 sidecar，再签外层 bundle，顺序不能反
codesign --force --timestamp=none -s "$IDENTITY" "$SRC/Contents/MacOS/snaplingo-ocr"
codesign --force --timestamp=none -s "$IDENTITY" "$SRC"

# ---------------------------------------------------------------- 安装

echo "[install] 安装到 ~/Applications"
pkill -f "$APP_NAME/Contents/MacOS/snaplingo" 2>/dev/null || true
sleep 1
mkdir -p ~/Applications
rm -rf ~/Applications/"$APP_NAME"
cp -R "$SRC" ~/Applications/

echo ""
echo "[done] 已安装 ~/Applications/$APP_NAME"
codesign -d -r- ~/Applications/"$APP_NAME" 2>&1 | grep designated || true
echo ""
echo "启动: open ~/Applications/$APP_NAME"
