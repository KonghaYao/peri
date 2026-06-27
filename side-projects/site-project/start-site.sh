#!/bin/bash
# site-project 启动脚本
set -e

PORT=23566
DIR="$(cd "$(dirname "$0")" && pwd)"

# 关闭旧进程
if lsof -ti:$PORT > /dev/null 2>&1; then
  echo "关闭已有服务 (port $PORT)..."
  lsof -ti:$PORT | xargs kill -9 2>/dev/null
  sleep 1
fi

echo "启动 site-project..."
cd "$DIR"
npm install --silent 2>/dev/null || true
nohup npx tsx src/server.ts > /tmp/site-project.log 2>&1 &
sleep 2

echo ""
echo "  http://localhost:$PORT"
echo ""
