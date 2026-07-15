#!/bin/bash

# PDF Reader 编译脚本（Rust + lopdf 版）
# 用法:
#   bash build.sh        # 默认 debug 模式：交叉编译(debug 快) → appcenter-cli install-local 安装到 fnOS 调试
#   bash build.sh -r     # release 模式：交叉编译(release 精简) → fnpack build 打包 fpk
#
# 流程：
#   1) 编译 Vue 前端 → 复制到 fpk 的 app/ui
#   2) 交叉编译 Rust 服务端到 aarch64 Linux(musl 静态) → 复制到 fpk 的 app/server/pdfserver
#      - debug   : cargo zigbuild            （编译快，二进制大，用于本地调试）
#      - release : cargo zigbuild --release  （opt/strip，二进制小，用于发布）
#   3) 收尾：
#      - debug   : appcenter-cli install-local  （直接安装到 fnOS 调试）
#      - release : fnpack build                 （打包 fpk 供分发）
#
# 交叉编译说明（在 macOS/arm64 上产出飞牛 NAS 的 aarch64-linux 二进制）：
#   首选 cargo-zigbuild（用 zig 做交叉链接，产出静态 musl 二进制，无 glibc 版本依赖）
#   回退 cross（需要 Docker）
# 依赖自检见下方 ensure_rust_toolchain。

set -e

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# -----------------------------------------------------------------------------
# 参数解析：默认 debug；-r/--release 切换到 release 打包 fpk
# -----------------------------------------------------------------------------
RELEASE=0
for arg in "$@"; do
    case "$arg" in
        -r|--release) RELEASE=1 ;;
        -h|--help)
            echo "用法: bash build.sh [-r]"
            echo "  (无参数)   debug 模式：交叉编译 debug 二进制并 appcenter-cli install-local 安装到 fnOS"
            echo "  -r|--release  release 模式：交叉编译 release 二进制并 fnpack build 打包 fpk"
            exit 0
            ;;
        *)
            echo -e "${RED}未知参数: $arg${NC}"
            echo "用法: bash build.sh [-r]"
            exit 1
            ;;
    esac
done

if [ "$RELEASE" = "1" ]; then
    MODE="release"
    CARGO_PROFILE_FLAG="--release"
    CARGO_PROFILE_DIR="release"
else
    MODE="debug"
    CARGO_PROFILE_FLAG=""
    CARGO_PROFILE_DIR="debug"
fi

echo -e "${GREEN}=== PDF Reader 编译脚本 (Rust + lopdf) — ${MODE} 模式 ===${NC}"

# 项目根目录
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR"
FNOS_APP_DIR="$PROJECT_ROOT/fnnas.pdfreader"
RUST_DIR="$PROJECT_ROOT/rustservice"

# 目标三元组：飞牛 NAS 为 aarch64 Linux；musl 静态链接避免 glibc 版本问题
RUST_TARGET="aarch64-unknown-linux-musl"

echo "项目根目录: $PROJECT_ROOT"
echo "fnOS 应用目录: $FNOS_APP_DIR"
echo "Rust 服务端目录: $RUST_DIR"
echo "交叉编译目标: $RUST_TARGET"
echo "构建模式: $MODE"

# 定位 cargo / rustup（优先 PATH，其次 ~/.cargo/bin）
CARGO="$(command -v cargo || echo "$HOME/.cargo/bin/cargo")"
RUSTUP="$(command -v rustup || echo "$HOME/.cargo/bin/rustup")"

# -----------------------------------------------------------------------------
# 工具链自检：确保能交叉编译到 aarch64 Linux
# -----------------------------------------------------------------------------
ensure_rust_toolchain() {
    if [ ! -x "$CARGO" ]; then
        echo -e "${RED}错误: 未找到 cargo，请先安装 Rust: https://rustup.rs${NC}"
        exit 1
    fi

    # 安装目标标准库
    if ! "$RUSTUP" target list --installed 2>/dev/null | grep -q "^${RUST_TARGET}$"; then
        echo "安装 Rust 目标 ${RUST_TARGET} ..."
        "$RUSTUP" target add "${RUST_TARGET}"
    fi

    # 选择交叉编译后端：cargo-zigbuild 优先，其次 cross
    BUILD_MODE=""
    if command -v cargo-zigbuild >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
        BUILD_MODE="zigbuild"
    elif command -v cross >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
        BUILD_MODE="cross"
    else
        echo -e "${RED}错误: 未找到可用的交叉编译后端。${NC}"
        echo -e "${YELLOW}请任选其一安装：${NC}"
        echo "  A) 推荐(无需 Docker)：brew install zig && cargo install cargo-zigbuild"
        echo "  B) 备选(需 Docker)  ：cargo install cross  （并启动 Docker）"
        exit 1
    fi
    echo -e "${GREEN}交叉编译后端: ${BUILD_MODE}${NC}"
}

# =============================================================================
# [Step 1/3] 编译 Vue 前端
# =============================================================================
echo ""
echo -e "${YELLOW}[Step 1/3] 编译 Vue 前端...${NC}"

cd "$PROJECT_ROOT/vueapp"

# 检查 node_modules 是否存在，不存在则安装依赖
if [ ! -d "node_modules" ]; then
    echo "安装 npm 依赖..."
    npm install
fi

echo "执行 npm run build..."
npm run build

if [ ! -d "dist" ]; then
    echo -e "${RED}错误: 构建失败，dist 目录不存在${NC}"
    exit 1
fi

echo -e "${GREEN}Vue 前端编译完成${NC}"

# 复制前端文件到 fnos 应用目录
echo ""
echo -e "${YELLOW}复制前端文件...${NC}"

UI_DIR="$FNOS_APP_DIR/app/ui"
rm -rf "$UI_DIR/assets"
# 复制 dist 目录内容，直接覆盖同名文件或文件夹
cp -r dist/* "$UI_DIR/"

echo -e "${GREEN}前端文件复制完成${NC}"

# =============================================================================
# [Step 2/3] 交叉编译 Rust 服务端 → aarch64 Linux
# =============================================================================
echo ""
echo -e "${YELLOW}[Step 2/3] 交叉编译 Rust 服务端 (${RUST_TARGET}, ${MODE})...${NC}"

ensure_rust_toolchain

cd "$RUST_DIR"

if [ "$BUILD_MODE" = "zigbuild" ]; then
    "$CARGO" zigbuild ${CARGO_PROFILE_FLAG} --target "${RUST_TARGET}"
else
    cross build ${CARGO_PROFILE_FLAG} --target "${RUST_TARGET}"
fi

BIN_OUT="$RUST_DIR/target/${RUST_TARGET}/${CARGO_PROFILE_DIR}/pdfserver"
if [ ! -f "$BIN_OUT" ]; then
    echo -e "${RED}错误: Rust 交叉编译失败，未找到 $BIN_OUT${NC}"
    exit 1
fi

# 校验产物架构（应为 aarch64/ARM64 ELF）
echo "产物信息:"
file "$BIN_OUT" || true
ARCH_OK="$(file "$BIN_OUT" 2>/dev/null | grep -Ei 'ELF.*(aarch64|ARM aarch64)' || true)"
if [ -z "$ARCH_OK" ]; then
    echo -e "${RED}错误: 产物不是 aarch64 Linux ELF，请检查交叉编译环境${NC}"
    exit 1
fi

# 复制到 fnos 应用目录
SERVER_DIR="$FNOS_APP_DIR/app/server"
mkdir -p "$SERVER_DIR"
cp -f "$BIN_OUT" "$SERVER_DIR/pdfserver"
chmod +x "$SERVER_DIR/pdfserver"

echo -e "${GREEN}Rust 服务端编译完成 → $SERVER_DIR/pdfserver（$(du -h "$SERVER_DIR/pdfserver" | cut -f1)）${NC}"

# =============================================================================
# [Step 3/3] 收尾：debug 安装到 fnOS / release 打包 fpk
# =============================================================================
echo ""
cd "$FNOS_APP_DIR"

if [ "$RELEASE" = "1" ]; then
    echo -e "${YELLOW}[Step 3/3] 打包 fpk...${NC}"
    fnpack build
    #appcenter-cli install-fpk fnnas.pdfreader.fpk
    echo ""
    echo -e "${GREEN}=== 编译完成 (release / fpk 已打包) ===${NC}"
else
    echo -e "${YELLOW}[Step 3/3] 安装到 fnOS 调试 (appcenter-cli install-local)...${NC}"
    appcenter-cli install-local
    echo ""
    echo -e "${GREEN}=== 编译完成 (debug / 已安装到 fnOS) ===${NC}"
fi

echo "应用目录: $FNOS_APP_DIR"
