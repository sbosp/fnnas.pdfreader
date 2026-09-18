#!/bin/bash

# PDF Reader 编译脚本（Go 后端 · 同进程原生 PDFium CGO）
#
# 用法:
#   bash build.sh                         # NAS aarch64 本机完整构建
#   PDFR_NATIVE_HOST=1 SKIP_UI=1 bash build.sh   # Mac 本机调试构建
#   SKIP_UI=1 bash build.sh               # 只重编后端
#
# 架构:
#   pdfserver      — CGO + libpdfium（同进程渲页，无 pdfium-worker）
#   lib/libpdfium.* — 运行时动态库

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${GREEN}=== PDF Reader 编译脚本（Go · 原生 PDFium）===${NC}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR"
FNOS_APP_DIR="$PROJECT_ROOT/fnnas.pdfreader"
GOSVC="$PROJECT_ROOT/goservice"
PDFIUM_DIR="${PDFIUM_DIR:-$GOSVC/third_party/pdfium}"

export PATH="$PATH:/opt/homebrew/bin:$HOME/go/bin:/usr/local/go/bin:/usr/local/bin"

echo "项目根目录: $PROJECT_ROOT"
echo "运行环境: $(uname -s) $(uname -m)"

if ! command -v go >/dev/null 2>&1; then
    echo -e "${RED}错误: 未找到 go${NC}"
    exit 1
fi
echo "Go 版本: $(go version)"

# ---------- Step 1: 前端 ----------
echo ""
if [ "${SKIP_UI:-0}" = "1" ]; then
    echo -e "${YELLOW}[Step 1/4] 跳过前端构建（SKIP_UI=1）${NC}"
else
    if ! command -v npm >/dev/null 2>&1; then
        echo -e "${RED}错误: 未找到 npm。可 SKIP_UI=1 bash build.sh${NC}"
        exit 1
    fi
    echo -e "${YELLOW}[Step 1/4] 编译 React 前端...${NC}"
    cd "$PROJECT_ROOT/reactapp"
    [ -d node_modules ] || npm install
    npm run build
    [ -d dist ] || { echo -e "${RED}前端构建失败${NC}"; exit 1; }
    echo -e "${GREEN}React 前端编译完成${NC}"
fi

# ---------- Step 2: 拷贝前端 ----------
echo ""
if [ "${SKIP_UI:-0}" = "1" ]; then
    echo -e "${YELLOW}[Step 2/4] 跳过前端拷贝${NC}"
else
    echo -e "${YELLOW}[Step 2/4] 拷贝前端产物...${NC}"
    UI_DIR="$FNOS_APP_DIR/app/ui"
    mkdir -p "$UI_DIR"
    rm -rf "$UI_DIR/assets"
    cp -r "$PROJECT_ROOT/reactapp/dist/"* "$UI_DIR/"
    echo -e "${GREEN}前端文件复制完成${NC}"
fi

# ---------- Step 3: PDFium SDK ----------
echo ""
echo -e "${YELLOW}[Step 3/4] 准备原生 PDFium...${NC}"
HOST_ARCH="$(uname -m)"
HOST_OS="$(uname -s)"

BUILD_HOST=0
if [ "$HOST_OS" = "Linux" ] && { [ "$HOST_ARCH" = "aarch64" ] || [ "$HOST_ARCH" = "arm64" ]; }; then
    BUILD_HOST=1
    FETCH_OS=linux
    FETCH_ARCH=arm64
elif [ "${PDFR_NATIVE_HOST:-0}" = "1" ]; then
    BUILD_HOST=1
    if [ "$HOST_OS" = "Darwin" ]; then FETCH_OS=mac; else FETCH_OS=linux; fi
    case "$HOST_ARCH" in
        arm64|aarch64) FETCH_ARCH=arm64 ;;
        x86_64|amd64)  FETCH_ARCH=x64 ;;
        *) echo -e "${RED}不支持架构 $HOST_ARCH${NC}"; exit 1 ;;
    esac
else
    # 默认目标：飞牛 linux/arm64
    FETCH_OS=linux
    FETCH_ARCH=arm64
    if [ "$HOST_OS" = "Darwin" ] || { [ "$HOST_ARCH" != "aarch64" ] && [ "$HOST_ARCH" != "arm64" ]; }; then
        if [ -z "${CC:-}" ] && ! command -v aarch64-linux-gnu-gcc >/dev/null 2>&1; then
            echo -e "${YELLOW}交叉编 pdfserver 需要 aarch64-linux-gnu-gcc（或在 NAS 本机构建）。${NC}"
            echo "  本机调试: PDFR_NATIVE_HOST=1 SKIP_UI=1 bash build.sh"
            echo "  NAS 构建: 在 aarch64 机器上直接 bash build.sh"
            exit 1
        fi
        export CC="${CC:-aarch64-linux-gnu-gcc}"
        export CXX="${CXX:-aarch64-linux-gnu-g++}"
    fi
fi

NEED_FETCH=1
if [ -f "$PDFIUM_DIR/pdfium.pc" ] && [ -d "$PDFIUM_DIR/lib" ]; then
    if [ "$FETCH_OS" = "mac" ] && [ -f "$PDFIUM_DIR/lib/libpdfium.dylib" ]; then NEED_FETCH=0; fi
    if [ "$FETCH_OS" = "linux" ] && ls "$PDFIUM_DIR/lib"/libpdfium.so* >/dev/null 2>&1; then NEED_FETCH=0; fi
fi
if [ "$NEED_FETCH" = "1" ]; then
    bash "$PROJECT_ROOT/scripts/fetch-pdfium.sh" "$FETCH_OS" "$FETCH_ARCH"
fi

export PKG_CONFIG_PATH="$PDFIUM_DIR${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}"
if ! pkg-config --exists pdfium; then
    echo -e "${RED}pkg-config 找不到 pdfium${NC}"
    exit 1
fi
echo "PDFium $(pkg-config --modversion pdfium)"

# ---------- Step 4: 编译 ----------
echo ""
cd "$GOSVC"
SERVER_DIR="$FNOS_APP_DIR/app/server"
mkdir -p "$SERVER_DIR/lib"

echo -e "${YELLOW}[Step 4/4] 编译 pdfserver（CGO 同进程 PDFium）...${NC}"

if [ "$FETCH_OS" = "mac" ]; then
    export CGO_LDFLAGS="${CGO_LDFLAGS:-} -Wl,-rpath,@loader_path/lib"
else
    export CGO_LDFLAGS="${CGO_LDFLAGS:-} -Wl,-rpath,\$ORIGIN/lib"
fi
export CGO_ENABLED=1
if [ "$BUILD_HOST" = "1" ]; then
    go build -trimpath -ldflags="-s -w" -o "$SERVER_DIR/pdfserver" .
else
    GOOS=linux GOARCH=arm64 go build -trimpath -ldflags="-s -w" -o "$SERVER_DIR/pdfserver" .
fi

if [ ! -f "$SERVER_DIR/pdfserver" ]; then
    echo -e "${RED}错误: Go 编译失败${NC}"
    exit 1
fi

if [ "$FETCH_OS" = "mac" ]; then
    install_name_tool -change './libpdfium.dylib' '@rpath/libpdfium.dylib' "$SERVER_DIR/pdfserver" 2>/dev/null || true
fi

chmod +x "$SERVER_DIR/pdfserver"
rm -f "$SERVER_DIR/pdfium-worker"
rm -f "$SERVER_DIR/lib"/libpdfium.*
cp -a "$PDFIUM_DIR/lib"/libpdfium.* "$SERVER_DIR/lib/"

echo "pdfserver:  $(ls -lh "$SERVER_DIR/pdfserver" | awk '{print $5}')"
echo "libpdfium: $(ls -lh "$SERVER_DIR/lib"/libpdfium.* | awk '{print $5}')"
echo -e "${GREEN}后端编译完成（无 pdfium-worker）${NC}"

if [ "$BUILD_HOST" = "1" ]; then
    cp -f "$SERVER_DIR/pdfserver" "$GOSVC/pdfserver"
    mkdir -p "$GOSVC/lib"
    cp -a "$PDFIUM_DIR/lib"/libpdfium.* "$GOSVC/lib/"
    chmod +x "$GOSVC/pdfserver"
    rm -f "$GOSVC/pdfium-worker"
fi

# ---------- 打包 ----------
echo ""
if command -v fnpack >/dev/null 2>&1; then
    echo -e "${YELLOW}[Pack] fnpack...${NC}"
    cd "$FNOS_APP_DIR"
    fnpack build
    if command -v appcenter-cli >/dev/null 2>&1; then
        appcenter-cli install-fpk fnnas.pdfreader.fpk
        echo -e "${GREEN}已安装到 fnOS${NC}"
    else
        echo "fpk: $FNOS_APP_DIR/fnnas.pdfreader.fpk"
    fi
else
    echo -e "${YELLOW}未检测到 fnpack，跳过打包。${NC}"
fi

echo ""
echo -e "${GREEN}=== 编译完成 ===${NC}"
echo "本机调试: PDFR_NATIVE_HOST=1 SKIP_UI=1 bash build.sh && cd goservice && ./pdfserver --port 18080"
