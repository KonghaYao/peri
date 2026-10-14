#!/usr/bin/env bash
# A2 侦察：在同一份 20 万行副本上做对照度量（生产 SQL vs rowid 下界 vs 假设索引）。
# 只读/只在一次性副本上建索引；不触碰仓库代码，也不触碰任何 Peri 会话库。
set -u

DB="${1:?用法: ab-probe.sh <db 路径>}"

Q1="SELECT message_id, content FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid"
Q2="SELECT message_id, truncated, excluded, projection FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 AND (truncated = 1 OR projection IS NOT NULL)"
Q3="SELECT message_id, rowid FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 ORDER BY rowid ASC"
V1="SELECT message_id, content FROM messages WHERE thread_id = 'probe-thread' AND excluded = 0 AND rowid >= 199998 ORDER BY rowid"

timed () {
  local label="$1" sql="$2" rep
  for rep in 1 2 3; do
    local real
    real=$( { /usr/bin/time -p sqlite3 "$DB" "$sql" > /dev/null; } 2>&1 | awk '/^real/{print $2}' )
    printf '  %-28s rep%s real=%ss\n' "$label" "$rep" "$real"
  done
}

plan () {
  local label="$1" sql="$2"
  echo "  [$label]"
  sqlite3 "$DB" ".eqp full" ".scanstats on" "$sql" 2>&1 \
    | grep -E 'SEARCH|SCAN|loops=' | sed 's/^/    /'
}

echo "=== 阶段 0：副本基线（无附加索引）==="
plan "Q1 生产原句" "EXPLAIN QUERY PLAN $Q1"
plan "Q1 scanstats（COUNT 包裹，避免打印 64KiB 正文）" "SELECT COUNT(*) FROM ($Q1)"
plan "Q2 scanstats" "SELECT COUNT(*) FROM ($Q2)"
plan "Q3 scanstats" "SELECT COUNT(*) FROM ($Q3)"
plan "V1 加 rowid 下界" "SELECT COUNT(*) FROM ($V1)"
timed "Q1 生产原句" "$Q1"
timed "Q2" "$Q2"
timed "Q3" "$Q3"
timed "V1 rowid 下界" "$V1"

echo
echo "=== 阶段 1：假设索引 A — CREATE INDEX idx_active_a ON messages(thread_id, excluded) ==="
sqlite3 "$DB" "CREATE INDEX idx_active_a ON messages(thread_id, excluded);"
plan "Q1 生产原句" "EXPLAIN QUERY PLAN $Q1"
plan "Q1 scanstats" "SELECT COUNT(*) FROM ($Q1)"
plan "Q2 scanstats" "SELECT COUNT(*) FROM ($Q2)"
plan "Q3 scanstats" "SELECT COUNT(*) FROM ($Q3)"
timed "Q1 生产原句" "$Q1"
timed "Q2" "$Q2"
timed "Q3" "$Q3"
sqlite3 "$DB" "DROP INDEX idx_active_a;"

echo
echo "=== 阶段 2：假设索引 B — CREATE INDEX idx_active_b ON messages(thread_id) WHERE excluded = 0 ==="
sqlite3 "$DB" "CREATE INDEX idx_active_b ON messages(thread_id) WHERE excluded = 0;"
plan "Q1 生产原句" "EXPLAIN QUERY PLAN $Q1"
plan "Q1 scanstats" "SELECT COUNT(*) FROM ($Q1)"
plan "Q2 scanstats" "SELECT COUNT(*) FROM ($Q2)"
plan "Q3 scanstats" "SELECT COUNT(*) FROM ($Q3)"
timed "Q1 生产原句" "$Q1"
timed "Q2" "$Q2"
timed "Q3" "$Q3"

echo
echo "=== 索引占用（dbstat）==="
sqlite3 "$DB" "SELECT name, SUM(pgsize) AS bytes, COUNT(*) AS pages FROM dbstat GROUP BY name ORDER BY bytes DESC;"
