#!/bin/bash
set -e

DOCS_DIR="${1:-$(cd "$(dirname "$0")/" && pwd)/}"
SCRIPT_DIR="$(cd "$(dirname "$0")/side-projects/site-project" && pwd)"

cd "$SCRIPT_DIR"

# 修复 node-pty 预编译二进制权限（npm 可能丢失 +x）
HELPER="node_modules/node-pty/prebuilds/darwin-arm64/spawn-helper"
[ -f "$HELPER" ] && chmod +x "$HELPER"

echo "🚀 Starting site-project on http://localhost:23566"
echo "📁 Docs directory: $DOCS_DIR"
npx tsx src/server.ts "$DOCS_DIR"
