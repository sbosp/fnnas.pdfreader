#!/usr/bin/env bash
# 下载 bblanchon/pdfium-binaries 到 goservice/third_party/pdfium
# 用法:
#   bash scripts/fetch-pdfium.sh              # 按本机 OS/ARCH
#   bash scripts/fetch-pdfium.sh linux arm64  # 指定目标（交叉打包用）
set -euo pipefail

PDFIUM_VER="${PDFIUM_VER:-7881}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DEST="${PDFIUM_DIR:-$ROOT/goservice/third_party/pdfium}"

OS_IN="${1:-}"
ARCH_IN="${2:-}"
if [ -z "$OS_IN" ]; then
  case "$(uname -s)" in
    Darwin) OS_IN=mac ;;
    Linux)  OS_IN=linux ;;
    *) echo "不支持的 OS: $(uname -s)"; exit 1 ;;
  esac
fi
if [ -z "$ARCH_IN" ]; then
  case "$(uname -m)" in
    arm64|aarch64) ARCH_IN=arm64 ;;
    x86_64|amd64)  ARCH_IN=x64 ;;
    *) echo "不支持的 ARCH: $(uname -m)"; exit 1 ;;
  esac
fi

# bblanchon 命名: mac-arm64, linux-arm64, linux-x64, mac-x64, win-x64 ...
ASSET="pdfium-${OS_IN}-${ARCH_IN}.tgz"
URL="https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F${PDFIUM_VER}/${ASSET}"

echo "下载 PDFium ${PDFIUM_VER}: $ASSET"
echo "→ $DEST"
rm -rf "$DEST"
mkdir -p "$DEST"
TMP="$(mktemp)"
curl -fsSL "$URL" -o "$TMP"
tar -C "$DEST" -xzf "$TMP"
rm -f "$TMP"

cat > "$DEST/pdfium.pc" <<EOF
prefix=$DEST
libdir=\${prefix}/lib
includedir=\${prefix}/include

Name: PDFium
Description: PDFium
Version: ${PDFIUM_VER}
Requires:

Libs: -L\${libdir} -lpdfium
Cflags: -I\${includedir}
EOF

install_name_tool -id '@rpath/libpdfium.dylib' "$DEST/lib/libpdfium.dylib" 2>/dev/null || true
echo "完成。PKG_CONFIG_PATH=$DEST"
export PKG_CONFIG_PATH="$DEST${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
pkg-config --modversion pdfium
pkg-config --libs --cflags pdfium
ls -la "$DEST/lib"
