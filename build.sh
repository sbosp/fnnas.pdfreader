#!/bin/bash

# PDF Reader 编译脚本（Python + PyMuPDF + PyInstaller 版）
# 用法:
#   bash build.sh        # 默认 debug 模式：编译前端 + PyInstaller 打包后端 → appcenter-cli install-local
#   bash build.sh -r     # release 模式：编译前端 + PyInstaller 打包后端 → fnpack build 打 fpk
#
# 关键约束（务必理解）：
#   PyInstaller **不能交叉编译**——产物架构 == 打包所在机器的架构/OS。
#   NAS 是 aarch64 Linux，所以「后端打包」这一步**必须在 aarch64 Linux 上执行**
#   （NAS 本机，或等架构 Linux 容器）。在 macOS / x86 上执行会得到无法在 NAS
#   运行的产物，脚本会检测并直接报错。
#
# 因此推荐工作流：
#   * 前端可在任意机器编译（本脚本 Step 1）
#   * 后端打包 + 打 fpk 在 NAS 上执行（本脚本 Step 2/3）
#   或直接整脚本都在 NAS 上跑。
#
# 流程：
#   1) 编译 Vue 前端 → 复制到 fpk 的 app/ui
#   2) PyInstaller 打包 pyservice/pdfserver.py → 单文件二进制 → app/server/pdfserver
#   3) 收尾：debug=appcenter-cli install-local / release=fnpack build

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# -----------------------------------------------------------------------------
# 参数解析
# -----------------------------------------------------------------------------
RELEASE=0
SKIP_FRONTEND=0
for arg in "$@"; do
    case "$arg" in
        -r|--release) RELEASE=1 ;;
        --skip-frontend) SKIP_FRONTEND=1 ;;
        -h|--help)
            echo "用法: bash build.sh [-r] [--skip-frontend]"
            echo "  (无参数)         debug：编译前端 + PyInstaller 打包后端 + appcenter-cli install-local"
            echo "  -r|--release     release：编译前端 + PyInstaller 打包后端 + fnpack build 打 fpk"
            echo "  --skip-frontend  跳过前端编译（仅重打后端，适合只改了 Python 时）"
            exit 0
            ;;
        *)
            echo -e "${RED}未知参数: $arg${NC}"
            echo "用法: bash build.sh [-r] [--skip-frontend]"
            exit 1
            ;;
    esac
done

if [ "$RELEASE" = "1" ]; then
    MODE="release"
else
    MODE="debug"
fi

echo -e "${GREEN}=== PDF Reader 编译脚本 (Python + PyInstaller) — ${MODE} 模式 ===${NC}"

# 项目根目录
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR"
FNOS_APP_DIR="$PROJECT_ROOT/fnnas.pdfreader"
PY_DIR="$PROJECT_ROOT/pyservice"

HOST_OS="$(uname -s)"
HOST_ARCH="$(uname -m)"

echo "项目根目录: $PROJECT_ROOT"
echo "fnOS 应用目录: $FNOS_APP_DIR"
echo "Python 服务端目录: $PY_DIR"
echo "宿主机: ${HOST_OS}/${HOST_ARCH}"
echo "构建模式: $MODE"

# -----------------------------------------------------------------------------
# [Step 1/3] 编译 Vue 前端
# -----------------------------------------------------------------------------
if [ "$SKIP_FRONTEND" = "1" ]; then
    echo ""
    echo -e "${YELLOW}[Step 1/3] 跳过前端编译（--skip-frontend）${NC}"
else
    echo ""
    echo -e "${YELLOW}[Step 1/3] 编译 Vue 前端...${NC}"

    cd "$PROJECT_ROOT/vueapp"

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

    echo ""
    echo -e "${YELLOW}复制前端文件...${NC}"
    UI_DIR="$FNOS_APP_DIR/app/ui"
    rm -rf "$UI_DIR/assets"
    cp -r dist/* "$UI_DIR/"
    echo -e "${GREEN}前端文件复制完成${NC}"
fi

# -----------------------------------------------------------------------------
# [Step 2/3] PyInstaller 打包 Python 服务端 → aarch64 Linux 单文件二进制
# -----------------------------------------------------------------------------
echo ""
echo -e "${YELLOW}[Step 2/3] PyInstaller 打包 Python 服务端 (${MODE})...${NC}"

# ---- 架构闸门：PyInstaller 不能交叉编译，必须在 aarch64 Linux 上打包 ----
if [ "$HOST_OS" != "Linux" ] || { [ "$HOST_ARCH" != "aarch64" ] && [ "$HOST_ARCH" != "arm64" ]; }; then
    echo -e "${RED}错误: 当前宿主机是 ${HOST_OS}/${HOST_ARCH}，PyInstaller 无法交叉编译出 aarch64 Linux 二进制。${NC}"
    echo -e "${YELLOW}后端打包这一步必须在 aarch64 Linux 上执行，请任选其一：${NC}"
    echo "  A) 直接在飞牛 NAS（aarch64 Linux）本机上运行本脚本"
    echo "  B) 用等架构 Linux 容器打包，例如："
    echo "       docker run --rm --platform linux/arm64 -v \"$PROJECT_ROOT\":/app -w /app/pyservice \\"
    echo "         python:3.12-slim bash -lc 'pip install -r requirements.txt && pyinstaller --clean --noconfirm pdfserver.spec'"
    echo "     然后把 pyservice/dist/pdfserver 复制到 $FNOS_APP_DIR/app/server/pdfserver"
    echo ""
    echo -e "${YELLOW}提示: 前端已编译完成（如未加 --skip-frontend）。到 NAS 上可加 --skip-frontend 只打后端。${NC}"
    exit 1
fi

cd "$PY_DIR"

# 定位 Python 与 pyinstaller：优先项目 venv，其次系统
PYBIN=""
if [ -x "$PY_DIR/venv/bin/python" ]; then
    PYBIN="$PY_DIR/venv/bin/python"
elif command -v python3 >/dev/null 2>&1; then
    PYBIN="$(command -v python3)"
else
    echo -e "${RED}错误: 未找到 python3${NC}"
    exit 1
fi
echo "使用 Python: $PYBIN ($($PYBIN --version 2>&1))"

# 确保依赖 + pyinstaller 就绪
if ! "$PYBIN" -c "import PyInstaller" 2>/dev/null; then
    echo "安装打包依赖（pyinstaller + requirements）..."
    "$PYBIN" -m pip install --upgrade pip
    "$PYBIN" -m pip install -r requirements.txt
fi

# 执行打包
rm -rf build dist
"$PYBIN" -m PyInstaller --clean --noconfirm pdfserver.spec

BIN_OUT="$PY_DIR/dist/pdfserver"
if [ ! -f "$BIN_OUT" ]; then
    echo -e "${RED}错误: PyInstaller 打包失败，未找到 $BIN_OUT${NC}"
    exit 1
fi

# 校验产物架构（应为 aarch64/ARM64 ELF）
echo "产物信息:"
file "$BIN_OUT" || true
ARCH_OK="$(file "$BIN_OUT" 2>/dev/null | grep -Ei 'ELF.*(aarch64|ARM aarch64)' || true)"
if [ -z "$ARCH_OK" ]; then
    echo -e "${RED}错误: 产物不是 aarch64 Linux ELF，请检查打包环境${NC}"
    exit 1
fi

# 复制到 fnos 应用目录
SERVER_DIR="$FNOS_APP_DIR/app/server"
mkdir -p "$SERVER_DIR"
cp -f "$BIN_OUT" "$SERVER_DIR/pdfserver"
chmod +x "$SERVER_DIR/pdfserver"

echo -e "${GREEN}Python 服务端打包完成 → $SERVER_DIR/pdfserver（$(du -h "$SERVER_DIR/pdfserver" | cut -f1)）${NC}"

# -----------------------------------------------------------------------------
# [Step 3/3] 收尾
# -----------------------------------------------------------------------------
echo ""
cd "$FNOS_APP_DIR"

if [ "$RELEASE" = "1" ]; then
    echo -e "${YELLOW}[Step 3/3] 打包 fpk...${NC}"
    fnpack build
    echo ""
    echo -e "${GREEN}=== 编译完成 (release / fpk 已打包) ===${NC}"
else
    echo -e "${YELLOW}[Step 3/3] 安装到 fnOS 调试 (appcenter-cli install-local)...${NC}"
    appcenter-cli install-local
    echo ""
    echo -e "${GREEN}=== 编译完成 (debug / 已安装到 fnOS) ===${NC}"
fi

echo "应用目录: $FNOS_APP_DIR"
