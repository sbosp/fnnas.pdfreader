#!/bin/bash

# PDF Reader 编译脚本
# 用法: bash build.sh

set -e

# 颜色输出
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}=== PDF Reader 编译脚本 ===${NC}"

# 项目根目录
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$SCRIPT_DIR"
FNOS_APP_DIR="$PROJECT_ROOT/fnnas.pdfreader"

echo "项目根目录: $PROJECT_ROOT"
echo "fnOS 应用目录: $FNOS_APP_DIR"

echo ""
echo -e "${YELLOW}[Step 1/3] 编译 Vue 前端...${NC}"

cd "$PROJECT_ROOT/vueapp"

# 检查 node_modules 是否存在，不存在则安装依赖
if [ ! -d "node_modules" ]; then
    echo "安装 npm 依赖..."
    npm install
fi

# 执行构建
echo "执行 npm run build..."
npm run build

# 检查 dist 目录
if [ ! -d "dist" ]; then
    echo -e "${RED}错误: 构建失败，dist 目录不存在${NC}"
    exit 1
fi

echo -e "${GREEN}Vue 前端编译完成${NC}"

# 复制前端文件到 fnos 应用目录
echo ""
echo -e "${YELLOW}[Step 2/3] 复制前端文件...${NC}"

UI_DIR="$FNOS_APP_DIR/app/ui"

# 复制 dist 目录内容 直接覆盖同名文件或文件夹
cp -r dist/* "$UI_DIR/"

echo -e "${GREEN}前端文件复制完成${NC}"

# 编译 Python 服务端
echo ""
echo -e "${YELLOW}[Step 3/3] 编译 Python 服务端...${NC}"

cd "$PROJECT_ROOT/pyservice"

# ====================== 1. 虚拟环境处理 ======================
VENV_DIR="./venv"
if [ -d "${VENV_DIR}" ]; then
    echo -e "${GREEN}检测到已有虚拟环境，激活 venv...${NC}"
    source "${VENV_DIR}/bin/activate"
else
    echo -e "${GREEN}未检测到 venv，创建全新虚拟环境...${NC}"
    python3 -m venv "${VENV_DIR}"
    source "${VENV_DIR}/bin/activate"
fi

# ====================== 2. 清华源安装依赖 ======================
echo -e "${GREEN}使用清华源安装 requirements.txt 依赖...${NC}"
python -m pip install --upgrade pip setuptools wheel -i https://pypi.tuna.tsinghua.edu.cn/simple
python -m pip install -r requirements.txt -i https://pypi.tuna.tsinghua.edu.cn/simple

# 执行 PyInstaller 打包
echo "执行 pyinstaller..."
pyinstaller -F -w --optimize 2 --strip pdfserver_flask.py

# 检查编译结果
if [ ! -f "dist/pdfserver_flask" ]; then
    echo -e "${RED}错误: PyInstaller 编译失败${NC}"
    exit 1
fi

# 复制到 fnos 应用目录
SERVER_DIR="$FNOS_APP_DIR/app/server"
mkdir -p "$SERVER_DIR"

# 复制可执行文件并重命名
cp -f dist/pdfserver_flask "$SERVER_DIR/pdfserver"

echo -e "${GREEN}Python 服务端编译完成${NC}"

# 安装到 fnOS
echo ""
echo -e "${YELLOW}[Install] 安装到 fnOS...${NC}"

cd "$FNOS_APP_DIR"

#fnpack build
#
#appcenter-cli install-fpk fnnas.pdfreader.fpk

echo "执行 appcenter-cli install-local..."
appcenter-cli install-local

echo ""
echo -e "${GREEN}=== 编译完成 ===${NC}"
echo "应用目录: $FNOS_APP_DIR"
