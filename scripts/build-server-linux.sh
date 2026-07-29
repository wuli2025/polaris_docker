#!/usr/bin/env bash
# 编出**能塞进线上容器**的 polaris-server 二进制(WSL 里跑)。
#
# 为什么不用普通 cargo build:WSL Ubuntu 24.04 是 glibc 2.39,线上容器是 Debian
# bookworm(2.36)——直编的二进制进容器报 `GLIBC_2.39 not found`,线上崩过一次。
# cargo-zigbuild 用 zig 当链接器,可以**指定目标 glibc 版本**,这是唯一稳妥姿势。
#
# 前置:cargo-zigbuild + zig(本机 zig 在 ~/zig/zig,不在 PATH)。
# 产物:src-tauri/target/x86_64-unknown-linux-gnu/release/polaris-server
set -euo pipefail

cd "$(dirname "$0")/../src-tauri"
export PATH="$HOME/zig:$HOME/.cargo/bin:$PATH"

echo "== zig: $(zig version 2>/dev/null || echo '找不到 zig') =="
cargo zigbuild --release \
  --target x86_64-unknown-linux-gnu.2.36 \
  -p polaris-cli --bin polaris-server \
  --features collab-net

BIN=target/x86_64-unknown-linux-gnu/release/polaris-server
ls -la "$BIN"
# 冒烟只验链接,**绝不能跑这个二进制**:它不认 --version,会直接起服务并永久挂住。
echo "== 依赖的最高 GLIBC 版本(必须 ≤ 2.36)=="
objdump -T "$BIN" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -3
