#!/bin/sh
# aarch64-unknown-linux-musl 交叉链接器包装（供 cargo 调用）
#
# 用 zig 作为交叉链接器：zig 自带 musl libc 与各架构支持，无需再装
# aarch64-linux-musl-gcc 工具链。Mac 上 `brew install zig` 即可。
exec zig cc -target aarch64-linux-musl "$@"
