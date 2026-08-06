#!/bin/bash

# PDF Reader 编译脚本（Go 后端版）
# 用法: bash build.sh
#
# 后端为 Go 单二进制：交叉编译产出 aarch64 Linux 静态链接产物，
# 无需在 NAS 本机打包（告别 PyInstaller 的 PEP668 / 下载源 / 不能交叉编译等限制）。
# 本脚本在 Mac 开发机或 NAS 上跑均可（只需装好 Go 与 Node）。

set -e

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}=== PDF Reader 编译脚本（Go 后端版）===${NC}"

# 项目根目录
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR"
FNOS_APP_DIR="$PROJECT_ROOT/fnnas.pdfreader"

# Go 环境（兼容 homebrew / 官方安装路径）
export PATH="$PATH:/opt/homebrew/bin:/usr/local/go/bin"
# 国内 Go module 代理（可用环境变量覆盖）
export GOPROXY="${GOPROXY:-https://goproxy.cn,direct}"

echo "项目根目录: $PROJECT_ROOT"
echo "fnOS 应用目录: $FNOS_APP_DIR"

# 检查 Go
if ! command -v go >/dev/null 2>&1; then
    echo -e "${RED}错误: 未找到 go，请先安装 Go（brew install go 或官网）${NC}"
    exit 1
fi
echo "Go 版本: $(go version)"

# ---------- Step 1: 编译 Vue 前端 ----------
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
    echo -e "${RED}错误: 前端构建失败，dist 目录不存在${NC}"
    exit 1
fi
echo -e "${GREEN}Vue 前端编译完成${NC}"

# ---------- Step 2: 拷贝前端产物 ----------
echo ""
echo -e "${YELLOW}[Step 2/3] 拷贝前端产物...${NC}"
UI_DIR="$FNOS_APP_DIR/app/ui"
rm -rf "$UI_DIR/assets"
cp -r dist/* "$UI_DIR/"
echo -e "${GREEN}前端文件复制完成${NC}"

# ---------- Step 3: 交叉编译 Go 后端 ----------
echo ""
echo -e "${YELLOW}[Step 3/3] 交叉编译 Go 后端 (linux/arm64)...${NC}"
cd "$PROJECT_ROOT/goservice"
SERVER_DIR="$FNOS_APP_DIR/app/server"
mkdir -p "$SERVER_DIR"
# CGO_ENABLED=0 纯静态链接，免依赖；ldflags -s -w 去符号表减小体积
CGO_ENABLED=0 GOOS=linux GOARCH=arm64 go build -ldflags="-s -w" -o "$SERVER_DIR/pdfserver" .
if [ ! -f "$SERVER_DIR/pdfserver" ]; then
    echo -e "${RED}错误: Go 编译失败${NC}"
    exit 1
fi
echo "后端二进制大小: $(ls -lh "$SERVER_DIR/pdfserver" | awk '{print $5}')"
echo -e "${GREEN}Go 后端编译完成${NC}"

# ---------- 打包安装（仅 fnOS 环境有 fnpack 时执行） ----------
echo ""
if command -v fnpack >/dev/null 2>&1; then
    echo -e "${YELLOW}[Install] fnpack 打包并安装到 fnOS...${NC}"
    cd "$FNOS_APP_DIR"
    fnpack build
    appcenter-cli install-fpk fnnas.pdfreader.fpk
    echo -e "${GREEN}已安装到 fnOS${NC}"
else
    echo -e "${YELLOW}未检测到 fnpack（当前非 fnOS 环境），跳过打包安装。${NC}"
    echo "已产出完整应用目录: $FNOS_APP_DIR"
    echo "将其同步到 NAS 后执行:"
    echo "  cd fnnas.pdfreader && fnpack build && appcenter-cli install-fpk fnnas.pdfreader.fpk"
fi

echo ""
echo -e "${GREEN}=== 编译完成 ===${NC}"
