#!/usr/bin/env bash
# 一键启动 Peri 默认 threads.db 的 sqlite-mcp 服务。
set -euo pipefail

PACKAGE_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
cd "$PACKAGE_DIR"

if [ -z "${HOME:-}" ]; then
  printf '错误：HOME 未设置，无法定位 Peri 默认数据库。\n' >&2
  exit 1
fi

DATABASE_PATH="$HOME/.peri/threads/threads.db"
if [ ! -f "$DATABASE_PATH" ]; then
  printf '错误：未找到 Peri 默认数据库文件：\n  %s\n' "$DATABASE_PATH" >&2
  printf '请先启动 Peri 生成该文件；本脚本不会创建数据库。\n' >&2
  exit 1
fi

if ! command -v node >/dev/null 2>&1; then
  printf '错误：未找到 node。\n' >&2
  exit 1
fi

if [ ! -x node_modules/.bin/tsc ] || [ ! -d node_modules/mcp-use ]; then
  printf '依赖未安装，按 package-lock.json 执行 npm ci...\n'
  npm ci
fi

npm run build
printf '正在启动 sqlite-mcp（数据库：%s，端口：31339）\n' "$DATABASE_PATH"
exec node dist/src/cli.js "$DATABASE_PATH" --port 31339
