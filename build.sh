#!/bin/bash

# PDF Reader 编译脚本（Rust 后端版）
#
# 用法:
#   bash build.sh              完整构建（前端 + 后端）
#   SKIP_UI=1 bash build.sh    只重编后端（前端产物已是最新时更快）
#
# 本脚本在两种环境都能跑，会自动选择编译方式：
#
#   A) Mac / x86 开发机 → 交叉编译到 aarch64-unknown-linux-musl（静态链接）
#      前置：rustup + `rustup target add aarch64-unknown-linux-musl`
#            `brew install zig`（用作 musl 交叉链接器）
#            Node（构建前端）
#
#   B) NAS 本机（aarch64 Linux）→ 直接本机编译，不需要 zig
#      前置：rustup（curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh）
#            gcc/cc（apt install build-essential）
#            Node（若需构建前端；否则用 SKIP_UI=1）
#      有 fnpack 时会自动打包并安装 fpk。

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${GREEN}=== PDF Reader 编译脚本（Rust 后端版）===${NC}"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR"
FNOS_APP_DIR="$PROJECT_ROOT/fnnas.pdfreader"
TARGET_TRIPLE="aarch64-unknown-linux-musl"

export PATH="$PATH:/opt/homebrew/bin:$HOME/.cargo/bin:/usr/local/bin"

echo "项目根目录: $PROJECT_ROOT"
echo "fnOS 应用目录: $FNOS_APP_DIR"
echo "运行环境: $(uname -s) $(uname -m)"

if ! command -v cargo >/dev/null 2>&1; then
    echo -e "${RED}错误: 未找到 cargo，请先安装 Rust${NC}"
    echo "  安装: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"
    exit 1
fi
echo "Rust 版本: $(cargo --version)"

# 允许跳过前端构建（NAS 上若只想重编后端，可 SKIP_UI=1 bash build.sh）
if [ "${SKIP_UI:-0}" != "1" ]; then
    if ! command -v npm >/dev/null 2>&1; then
        echo -e "${RED}错误: 未找到 npm（构建前端需要 Node）。${NC}"
        echo "  若前端产物已是最新，可跳过前端: SKIP_UI=1 bash build.sh"
        exit 1
    fi
    echo "Node 版本: $(node --version 2>/dev/null)"
fi

# ---------- Step 1: 编译 React 前端 ----------
echo ""
if [ "${SKIP_UI:-0}" = "1" ]; then
    echo -e "${YELLOW}[Step 1/3] 跳过前端构建（SKIP_UI=1）${NC}"
else
    echo -e "${YELLOW}[Step 1/3] 编译 React 前端...${NC}"
    cd "$PROJECT_ROOT/reactapp"
    if [ ! -d "node_modules" ]; then
        echo "安装 npm 依赖..."
        npm install
    fi
    npm run build
    if [ ! -d "dist" ]; then
        echo -e "${RED}错误: 前端构建失败，dist 目录不存在${NC}"
        exit 1
    fi
    echo -e "${GREEN}React 前端编译完成${NC}"
fi

# ---------- Step 2: 拷贝前端产物 ----------
echo ""
if [ "${SKIP_UI:-0}" = "1" ]; then
    echo -e "${YELLOW}[Step 2/3] 跳过前端拷贝（SKIP_UI=1）${NC}"
else
    echo -e "${YELLOW}[Step 2/3] 拷贝前端产物...${NC}"
    UI_DIR="$FNOS_APP_DIR/app/ui"
    mkdir -p "$UI_DIR"
    rm -rf "$UI_DIR/assets"   # 清旧 assets，避免哈希孤儿文件堆积
    cp -r dist/* "$UI_DIR/"
    echo -e "${GREEN}前端文件复制完成${NC}"
fi

# ---------- Step 3: 编译 Rust 后端 ----------
echo ""
cd "$PROJECT_ROOT/rustservice"
SERVER_DIR="$FNOS_APP_DIR/app/server"
mkdir -p "$SERVER_DIR"

HOST_ARCH="$(uname -m)"
HOST_OS="$(uname -s)"

if [ "$HOST_OS" = "Linux" ] && { [ "$HOST_ARCH" = "aarch64" ] || [ "$HOST_ARCH" = "arm64" ]; }; then
    # 在 NAS 本机（aarch64 Linux）：直接本机编译，无需 zig / 交叉工具链。
    # 显式指定 gnu target 并清空该 target 的 linker 配置，避免命中
    # .cargo/config.toml 里为交叉编译准备的 musl+zig 链接器设置。
    NATIVE_TRIPLE="aarch64-unknown-linux-gnu"
    echo -e "${YELLOW}[Step 3/3] 本机编译 Rust 后端 ($NATIVE_TRIPLE)...${NC}"
    if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
        echo -e "${RED}错误: 未找到 C 链接器（cc/gcc）。Debian/fnOS 请先安装: apt install build-essential${NC}"
        exit 1
    fi
    if ! rustup target list --installed 2>/dev/null | grep -q "$NATIVE_TRIPLE"; then
        rustup target add "$NATIVE_TRIPLE" 2>/dev/null || true
    fi
    cargo build --release --target "$NATIVE_TRIPLE"
    BIN_PATH="target/$NATIVE_TRIPLE/release/pdfserver"
    # 个别环境未装 gnu target 时回退到默认 target 产物
    if [ ! -f "$BIN_PATH" ] && [ -f "target/release/pdfserver" ]; then
        BIN_PATH="target/release/pdfserver"
    fi
else
    # 开发机交叉编译到 aarch64 musl（静态链接，NAS 免运行时依赖）
    echo -e "${YELLOW}[Step 3/3] 交叉编译 Rust 后端 ($TARGET_TRIPLE)...${NC}"
    if ! rustup target list --installed 2>/dev/null | grep -q "$TARGET_TRIPLE"; then
        echo "添加编译目标 $TARGET_TRIPLE ..."
        rustup target add "$TARGET_TRIPLE"
    fi
    if ! command -v zig >/dev/null 2>&1; then
        echo -e "${RED}错误: 未找到 zig（用作 musl 交叉链接器）。请执行: brew install zig${NC}"
        exit 1
    fi
    chmod +x zig-cc-aarch64-musl.sh
    cargo build --release --target "$TARGET_TRIPLE"
    BIN_PATH="target/$TARGET_TRIPLE/release/pdfserver"
fi

if [ ! -f "$BIN_PATH" ]; then
    echo -e "${RED}错误: Rust 编译失败，未找到 $BIN_PATH${NC}"
    exit 1
fi
cp "$BIN_PATH" "$SERVER_DIR/pdfserver"
chmod +x "$SERVER_DIR/pdfserver"
echo "后端二进制大小: $(ls -lh "$SERVER_DIR/pdfserver" | awk '{print $5}')"
echo -e "${GREEN}Rust 后端编译完成${NC}"

# ---------- 打包 / 安装 ----------
# fnpack 与 appcenter-cli 分开判断：有 fnpack 就打包，装不装取决于 appcenter-cli
# （Mac 上可能只装了 fnpack，此时打完包提示手动安装即可，不应让脚本失败）。
echo ""
if command -v fnpack >/dev/null 2>&1; then
    echo -e "${YELLOW}[Pack] fnpack 打包...${NC}"
    cd "$FNOS_APP_DIR"
    fnpack build
    FPK_FILE="$FNOS_APP_DIR/fnnas.pdfreader.fpk"

    if command -v appcenter-cli >/dev/null 2>&1; then
        echo -e "${YELLOW}[Install] 安装到 fnOS...${NC}"
        appcenter-cli install-fpk fnnas.pdfreader.fpk
        echo -e "${GREEN}已安装到 fnOS${NC}"
    else
        echo -e "${YELLOW}未检测到 appcenter-cli，已完成打包但未安装。${NC}"
        echo "fpk 位置: $FPK_FILE"
        echo "在 NAS 上执行安装:"
        echo "  appcenter-cli install-fpk fnnas.pdfreader.fpk"
        echo "（或在 fnOS 应用中心手动上传该 fpk）"
    fi
else
    echo -e "${YELLOW}未检测到 fnpack，跳过打包。${NC}"
    echo "已产出完整应用目录: $FNOS_APP_DIR"
    echo "将其同步到 NAS 后执行:"
    echo "  cd fnnas.pdfreader && fnpack build && appcenter-cli install-fpk fnnas.pdfreader.fpk"
fi

echo ""
echo -e "${GREEN}=== 编译完成 ===${NC}"
