#!/usr/bin/env python3
"""
从 threads.db 中提取指定日期的对话，转为精简纯文本。

用法:
  python3 extract_daily.py <YYYY-MM-DD> [--db ~/.peri/threads/threads.db] [--out /tmp/learn-day-YYYY-MM-DD.txt]

输出格式：
  === Thread: <thread_id> ===
  标题: <title>
  目录: <cwd>
  消息数: <N>

  [HH:MM:SS] 用户:
  <用户消息原文>

  [HH:MM:SS] 助手:
  <文本回复原文>

  [HH:MM:SS] >> Read src/foo.rs → 成功

  [HH:MM:SS] >> Edit src/foo.rs → ✗ 失败: old_string not found

  [HH:MM:SS] 助手:
  <文本回复原文>

过滤规则:
- 跳过 reasoning/thinking 块（体积大，通常是内部思考）
- 成功且无特殊输出的工具调用只显示一行摘要
- 失败的工具调用显示错误信息
- 工具结果和长消息显式首尾截断，并把截断计入完整性统计
- 连续相同的工具调用（如反复 Read 同一文件）合并为 "连续 N 次 Read xxx"
"""

import sqlite3
import json
import sys
import os
import argparse
import hashlib
import ntpath
from datetime import date, datetime, timedelta
from pathlib import Path
import re

# ANSI 转义序列正则（终端颜色/样式代码）
ANSI_RE = re.compile(r'\x1b\[[0-9;]*[a-zA-Z]')
SENSITIVE_PATTERNS = (
    re.compile(r"(?i)([\"']?authorization[\"']?\s*[:=]\s*[\"']?(?:(?:bearer|basic)\s+)?)[^\s,;\"'}]+"),
    re.compile(r"(?i)([\"']?(?:api[_-]?(?:key|token)|secret[_-]?(?:key|token)|access[_-]?token|password|client[_-]?secret|aws[_-]?secret[_-]?access[_-]?key|connection[_-]?string|database[_-]?url)[\"']?\s*[:=]\s*[\"']?)[^\s,;\"'}]+"),
    re.compile(r"(?i)\b([a-z][a-z0-9+.-]*://[^:/\s]+:)[^@\s/]+@"),
    re.compile(r"\b(?:sk|pk-lf|sk-lf)-[A-Za-z0-9_-]{8,}\b"),
    re.compile(r"\bgh[pousr]_[A-Za-z0-9]{20,}\b"),
    re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{12,}\b"),
    re.compile(r"\bAKIA[A-Z0-9]{16}\b"),
    re.compile(r"\bAIza[0-9A-Za-z_-]{20,}\b"),
)
MAX_PLAIN_TEXT_CHARS = 20_000
MAX_MESSAGE_TEXT_CHARS = 50_000
MAX_TOOL_RESULT_CHARS = 2_000
TRUNCATION_MARKER = "[TRUNCATED"
PARSE_FAILURE_MARKER = "[MESSAGE_PARSE_FAILED]"


def strip_ansi(text):
    """移除 ANSI 转义序列"""
    return ANSI_RE.sub('', text)


def redact_sensitive(text):
    """对提取物做保守脱敏，不把原始凭据写入临时目录。"""
    result = text
    for pattern in SENSITIVE_PATTERNS:
        if pattern.groups:
            result = pattern.sub(r"\1[REDACTED]", result)
        else:
            result = pattern.sub("[REDACTED]", result)
    return result


def truncate_text(text, limit, label):
    """显式截断文本并保留首尾，返回 (文本, 是否截断)。"""
    text = redact_sensitive(text)
    if len(text) <= limit:
        return text, False
    tail_size = min(1_000, limit // 4)
    head_size = limit - tail_size
    omitted = len(text) - head_size - tail_size
    marker = f"\n[{TRUNCATION_MARKER[1:]} {label}: omitted {omitted} chars]\n"
    return text[:head_size] + marker + text[-tail_size:], True


def _is_windows_path(path):
    return bool(re.match(r"^[A-Za-z]:[\\/]", path)) or path.startswith("\\\\")


def normalize_cwd(cwd):
    """规范化 cwd；识别历史中的 Windows 路径，不依赖当前宿主平台。"""
    expanded = os.path.expanduser(cwd)
    if _is_windows_path(expanded):
        return ntpath.normcase(ntpath.normpath(expanded))
    return os.path.normcase(os.path.normpath(os.path.abspath(expanded)))


def cwd_matches(candidate, project_root):
    """只匹配项目本身或其真实子目录，避免 /repo/foo 命中 /repo/foobar。"""
    try:
        candidate_normalized = normalize_cwd(candidate)
        root_normalized = normalize_cwd(project_root)
    except (TypeError, ValueError):
        return False
    path_module = ntpath if _is_windows_path(root_normalized) else os.path
    try:
        return path_module.commonpath((candidate_normalized, root_normalized)) == root_normalized
    except ValueError:
        return False


def _cwd_sql_clause(cwd):
    if not cwd:
        return "", []
    return "AND cwd_is_within(cwd, ?) = 1", [normalize_cwd(cwd)]


def connect_readonly(db_path):
    """创建只读连接，并注册跨平台 cwd 路径边界函数。"""
    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
    conn.row_factory = sqlite3.Row
    conn.create_function("cwd_is_within", 2, lambda candidate, root: int(cwd_matches(candidate, root)))
    return conn


def write_private_text(path, content):
    """以 0600 写入敏感提取物，不放宽既存父目录权限。"""
    path = Path(path)
    parent = path.parent
    missing_parents = []
    current = parent
    while not current.exists() and current != current.parent:
        missing_parents.append(current)
        current = current.parent
    parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    for created_parent in missing_parents:
        os.chmod(created_parent, 0o700)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(content)
    os.chmod(path, 0o600)


def get_db_path():
    """获取默认数据库路径"""
    home = os.path.expanduser("~")
    return os.path.join(home, ".peri", "threads", "threads.db")


def query_active_days(db_path, days=7, cwd=None, today=None):
    """查询含今天在内最近 N 个自然日期中有活跃 thread 的日期。

    Args:
        db_path: SQLite 数据库路径
        days: 自然日期数量（默认 7，含今天）
        cwd: 项目目录过滤（可选，不传则不限制项目）
        today: 查询基准日期（可选，用于确定性测试）

    Returns:
        list[dict]: [{"day": "YYYY-MM-DD", "thread_count": N, "total_msgs": N}, ...]
    """
    if days < 1:
        raise ValueError("days 必须大于 0")

    today = today or date.today()
    start_date = (today - timedelta(days=days - 1)).isoformat()
    end_date = (today + timedelta(days=1)).isoformat()

    conn = connect_readonly(db_path)
    cur = conn.cursor()

    cwd_clause, cwd_params = _cwd_sql_clause(cwd)
    params = [start_date, end_date, *cwd_params]

    cur.execute(f"""
        SELECT date(updated_at) as day,
               COUNT(*) as thread_count,
               SUM(message_count) as total_msgs
        FROM threads
        WHERE updated_at >= ?
          AND updated_at < ?
          AND message_count >= 3
          AND hidden = 0
          {cwd_clause}
        GROUP BY day
        ORDER BY day DESC
    """, params)

    rows = cur.fetchall()
    conn.close()
    return [dict(r) for r in rows]


def format_timestamp(ts_str):
    """将 ISO 时间戳转为 HH:MM:SS"""
    try:
        dt = datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
        return dt.strftime("%H:%M:%S")
    except (ValueError, AttributeError):
        return ts_str[:19] if ts_str else "??:??:??"


def parse_message(row):
    """解析消息行，并返回显式的截断与解析失败统计。"""
    msg_id, role, raw = row
    stats = {"truncations": 0, "parse_failures": 0}
    text_limit = MAX_TOOL_RESULT_CHARS if role == "tool" else MAX_PLAIN_TEXT_CHARS
    try:
        content = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        stats["parse_failures"] += 1
        return msg_id, role, PARSE_FAILURE_MARKER, None, False, stats

    # content 可能是字符串或 BaseMessage 字典
    if isinstance(content, str):
        text, truncated = truncate_text(content, text_limit, "plain message")
        stats["truncations"] += int(truncated)
        return msg_id, role, text, None, False, stats
    if not isinstance(content, dict):
        stats["parse_failures"] += 1
        return msg_id, role, PARSE_FAILURE_MARKER, None, False, stats

    text_parts = []
    tool_calls = []
    is_error = False

    # 处理 content 数组
    content_list = content.get("content", [])
    if isinstance(content_list, str):
        # 工具错误消息：content 是纯错误字符串
        is_error = content.get("is_error", False)
        text, truncated = truncate_text(content_list, text_limit, "message content")
        stats["truncations"] += int(truncated)
        return msg_id, role, text, None, is_error, stats
    if not isinstance(content_list, list):
        content_list = []
        text_parts.append(PARSE_FAILURE_MARKER)
        stats["parse_failures"] += 1

    for block in content_list:
        if not isinstance(block, dict):
            text_parts.append(PARSE_FAILURE_MARKER)
            stats["parse_failures"] += 1
            continue
        block_type = block.get("type", "")

        if block_type == "text":
            text = block.get("text", "")
            if isinstance(text, str) and text:
                text_parts.append(text)
            elif not isinstance(text, str):
                text_parts.append(PARSE_FAILURE_MARKER)
                stats["parse_failures"] += 1

        elif block_type == "tool_use":
            name = block.get("name", "unknown")
            tid = block.get("id", "")
            inp = block.get("input", {})
            # 精简参数摘要
            if isinstance(inp, dict):
                if name == "Read":
                    param_summary = inp.get("file_path", "") or inp.get("path", "") or inp.get("filePath", "")
                elif name == "Edit":
                    param_summary = f"{inp.get('file_path', '')}"
                elif name == "Write":
                    param_summary = f"{inp.get('file_path', '')}"
                elif name == "Bash":
                    cmd = inp.get("command", "")
                    cmd = cmd if isinstance(cmd, str) else str(cmd)
                    param_summary = " ".join(line.strip() for line in cmd.split("\n") if line.strip())
                elif name == "Grep":
                    param_summary = f"pattern={inp.get('pattern', '')}"
                elif name == "Glob":
                    param_summary = inp.get("pattern", "")
                elif name == "WebFetch":
                    param_summary = inp.get("url", "")
                elif name == "Agent":
                    param_summary = f"type={inp.get('subagent_type', '')}: {str(inp.get('description', ''))[:80]}"
                elif name == "WebSearch":
                    param_summary = inp.get("query", "")
                elif name == "TodoWrite":
                    param_summary = "update todo list"
                else:
                    # 通用参数摘要 (取前 2 个 key)
                    keys = list(inp.keys())[:2]
                    param_summary = ", ".join(f"{k}={str(inp[k])[:60]}" for k in keys)
            else:
                param_summary = str(inp)[:80]

            param_summary, truncated = truncate_text(str(param_summary), 400, "tool input")
            stats["truncations"] += int(truncated)
            tool_calls.append({
                "id": tid,
                "name": name,
                "summary": param_summary
            })

        elif block_type == "tool_result":
            tc = block.get("content", "")
            is_error = block.get("is_error", False)
            if isinstance(tc, list):
                # tool_result content 可能是 ContentBlock 数组
                tc_texts = []
                for sub in tc:
                    if isinstance(sub, dict) and sub.get("type") == "text" and isinstance(sub.get("text", ""), str):
                        tc_texts.append(sub.get("text", ""))
                    else:
                        stats["parse_failures"] += 1
                        tc_texts.append(PARSE_FAILURE_MARKER)
                tc = "\n".join(tc_texts)
            if isinstance(tc, str):
                tc, truncated = truncate_text(tc, MAX_TOOL_RESULT_CHARS, "tool result")
                stats["truncations"] += int(truncated)
            else:
                tc = PARSE_FAILURE_MARKER
                stats["parse_failures"] += 1

            # 工具结果会被 later 合并到 tool_calls 条目中
            for tc_item in tool_calls:
                if tc_item.get("id") == block.get("tool_use_id", ""):
                    tc_item["result"] = tc
                    tc_item["is_error"] = block.get("is_error", False)

        elif block_type == "reasoning":
            # 跳过 reasoning block（体积大且非必要）
            pass
        else:
            text_parts.append(f"{PARSE_FAILURE_MARKER} unsupported content block")
            stats["parse_failures"] += 1

    text = "\n".join(text_parts) if text_parts else ""
    text, truncated = truncate_text(text, MAX_MESSAGE_TEXT_CHARS, "message text")
    stats["truncations"] += int(truncated)
    return msg_id, role, text, tool_calls, is_error, stats


def thread_file_stem(thread_id):
    """生成可读且抗短前缀碰撞的文件名。"""
    readable = re.sub(r"[^A-Za-z0-9._-]", "_", thread_id[:12]) or "thread"
    digest = hashlib.sha256(thread_id.encode("utf-8")).hexdigest()[:8]
    return f"{readable}-{digest}"


def _format_thread(t, cur):
    """处理单个 thread，返回文件 stem、文本与完整性统计。"""
    thread_id = t["id"]
    thread_id_short = thread_file_stem(thread_id)

    lines = []
    lines.append(f"=== Thread: {thread_id} ===")
    lines.append(f"标题: {redact_sensitive(t['title'] or '(无标题)')}")
    lines.append(f"目录: {redact_sensitive(t['cwd'])}")
    lines.append(f"时间: {t['created_at'][:19]} ~ {t['updated_at'][:19]}")
    lines.append(f"消息数: {t['message_count']}")
    lines.append("")

    # 提取该 thread 的消息
    cur.execute("""
        SELECT message_id, role, content
        FROM messages
        WHERE thread_id = ?
        ORDER BY message_id ASC
    """, (thread_id,))

    messages = cur.fetchall()

    # 解析并合并消息
    parsed = []
    pending_tool_result = {}
    last_assistant_tool_calls = []
    truncation_count = 0
    parse_failure_count = 0

    for msg in messages:
        msg_id, role, text, tool_calls, is_error, stats = parse_message(msg)
        truncation_count += stats["truncations"]
        parse_failure_count += stats["parse_failures"]

        if role == "assistant" and tool_calls:
            last_assistant_tool_calls = tool_calls
            if text:
                parsed.append(("assistant_text", text))
            continue
        elif role == "tool":
            tc_id = None
            try:
                content_obj = json.loads(msg[2]) if isinstance(msg[2], str) else msg[2]
                if isinstance(content_obj, dict):
                    tc_id = content_obj.get("tool_call_id", "")
            except (json.JSONDecodeError, TypeError):
                pass
            if tc_id:
                pending_tool_result[tc_id] = {
                    "is_error": is_error,
                    "text": text
                }
            if last_assistant_tool_calls:
                all_collected = all(
                    tc.get("id") in pending_tool_result
                    for tc in last_assistant_tool_calls
                )
                if all_collected:
                    parsed.append(("tool_results", last_assistant_tool_calls, pending_tool_result.copy()))
                    last_assistant_tool_calls = []
                    pending_tool_result = {}
            continue

        if role == "assistant" and not tool_calls:
            if text:
                parsed.append(("assistant_text", text))
        elif role == "user":
            parsed.append(("user_text", text))
        elif role == "system":
            pass

    # flush 未合并的 tool_calls
    if last_assistant_tool_calls:
        parsed.append(("tool_results", last_assistant_tool_calls, pending_tool_result))

    # 构建有序列表
    ordered = []
    for entry in parsed:
        if entry[0] == "tool_results":
            tool_list = entry[1]
            results = entry[2]
            for tc in tool_list:
                tid = tc.get("id", "")
                tr = results.get(tid, {})
                ordered.append({
                    "type": "tool_call",
                    "name": tc["name"],
                    "summary": tc["summary"],
                    "is_error": tr.get("is_error", False),
                    "text": tr.get("text", ""),
                })
        elif entry[0] in ("assistant_text", "user_text"):
            ordered.append({"type": entry[0], "text": entry[1]})

    # 去重连续相同工具调用
    deduped = []
    error_count = 0
    i = 0
    while i < len(ordered):
        entry = ordered[i]
        if entry["type"] == "tool_call":
            count = 1
            results = [entry]
            j = i + 1
            while j < len(ordered) and ordered[j]["type"] == "tool_call" and ordered[j]["name"] == entry["name"] and ordered[j]["summary"] == entry["summary"]:
                count += 1
                results.append(ordered[j])
                j += 1
            last = results[-1]
            if last.get("is_error"):
                error_count += 1
            if count > 1:
                deduped.append({
                    "type": "dup_tool",
                    "count": count,
                    "name": entry["name"],
                    "summary": entry["summary"],
                    "is_error": last["is_error"],
                    "text": last["text"],
                })
            else:
                deduped.append(last)
            i = j
        else:
            deduped.append(entry)
            i += 1

    # 格式化输出
    for entry in deduped:
        if entry["type"] == "user_text":
            lines.append("[用户]:")
            for line in entry["text"].split("\n"):
                lines.append(f"  {line}")
            lines.append("")

        elif entry["type"] == "assistant_text":
            lines.append("[助手]:")
            for line in entry["text"].split("\n"):
                lines.append(f"  {line}")
            lines.append("")

        elif entry["type"] == "tool_call":
            lines.append(_format_tool_line(entry))
            lines.append("")

        elif entry["type"] == "dup_tool":
            line = _format_tool_line(entry)
            lines.append(f"  [连续 {entry['count']} 次] {line.strip()}")
            lines.append("")

    lines.append("---")
    lines.append("")

    return thread_id_short, lines, error_count, len(messages), truncation_count, parse_failure_count


def _query_threads_for_date(cur, date_str, cwd=None):
    """按 [day, next_day) 查询 thread，复用 cwd 路径边界。"""
    date_start = datetime.fromisoformat(date_str).strftime("%Y-%m-%dT00:00:00")
    date_end = (datetime.fromisoformat(date_str) + timedelta(days=1)).strftime("%Y-%m-%dT00:00:00")
    cwd_clause, cwd_params = _cwd_sql_clause(cwd)
    cur.execute(f"""
        SELECT id, title, cwd, created_at, updated_at, message_count
        FROM threads
        WHERE updated_at >= ? AND updated_at < ?
          AND message_count >= 3
          AND hidden = 0
          {cwd_clause}
        ORDER BY updated_at ASC
    """, [date_start, date_end, *cwd_params])
    return cur.fetchall()


def extract_date(date_str, db_path, output_path, cwd=None):
    """提取指定日期的所有 thread 对话（合并到一个文件）"""
    conn = connect_readonly(db_path)
    cur = conn.cursor()
    threads = _query_threads_for_date(cur, date_str, cwd)

    if not threads:
        conn.close()
        write_private_text(output_path, f"# {date_str}: 当天无活跃对话记录\n")
        return 0, {}

    lines = []
    lines.append(f"# 对话历史提取 — {date_str}")
    lines.append(f"# 共 {len(threads)} 个活跃线程")
    lines.append("")
    lines.append("---")
    lines.append("")

    for t in threads:
        _, thread_lines, _, _, _, _ = _format_thread(t, cur)
        lines.extend(thread_lines)

    conn.close()

    write_private_text(output_path, "\n".join(lines))

    return len(threads), {}


def extract_date_by_thread(date_str, db_path, out_dir, cwd=None):
    """提取指定日期的所有 thread 对话（每个 thread 单独一个文件）。

    Args:
        date_str: 日期字符串 YYYY-MM-DD
        db_path: SQLite 数据库路径
        out_dir: 输出目录
        cwd: 项目目录过滤（可选，不传则不限制项目）

    Returns:
        (thread_count, {thread_id_short: {"path": str, "size_kb": float, "msgs": int, "errors": int, "title": str, "cwd": str}})
    """
    conn = connect_readonly(db_path)
    cur = conn.cursor()
    threads = _query_threads_for_date(cur, date_str, cwd)

    os.makedirs(out_dir, exist_ok=True)
    os.chmod(out_dir, 0o700)

    if not threads:
        conn.close()
        return 0, {}

    results = {}
    for t in threads:
        tid_short, formatted_lines, error_count, msg_count, truncation_count, parse_failure_count = _format_thread(t, cur)
        filename = f"{tid_short}.txt"
        filepath = os.path.join(out_dir, filename)

        write_private_text(filepath, "\n".join(formatted_lines))

        size_kb = os.path.getsize(filepath) / 1024
        results[tid_short] = {
            "path": filepath,
            "filename": filename,
            "size_kb": size_kb,
            "msgs": msg_count,
            "errors": error_count,
            "truncations": truncation_count,
            "parse_failures": parse_failure_count,
            "title": redact_sensitive((t["title"] or "(无标题)")[:60]),
            "cwd": redact_sensitive(t["cwd"]),
            "thread_id": t["id"],
        }

    # 写索引文件
    index_path = os.path.join(out_dir, "_index.txt")
    index_lines = [
        f"# {date_str} — {len(threads)} threads",
        "",
        f"{'File':<30} {'Msg':>5} {'Err':>4} {'Trunc':>5} {'Parse':>5} {'KB':>6}  Title",
        "-" * 104,
    ]
    for tid_short in results:
        r = results[tid_short]
        index_lines.append(f"{r['filename']:<30} {r['msgs']:>5} {r['errors']:>4} {r['truncations']:>5} {r['parse_failures']:>5} {r['size_kb']:>6.0f}  {r['title']}")
    cwds = set(r.get("cwd", "?") for r in results.values())
    if len(cwds) > 1:
        index_lines.extend(["", "多项目目录:"])
        for c in sorted(cwds):
            count = sum(1 for r in results.values() if r.get("cwd") == c)
            index_lines.append(f"  {c} ({count} threads)")
    write_private_text(index_path, "\n".join(index_lines) + "\n")

    conn.close()
    return len(threads), results


def _format_tool_line(entry):
    """格式化单个工具调用行"""
    name = entry["name"]
    summary = entry["summary"]
    is_err = entry.get("is_error", False)
    text = strip_ansi(entry.get("text", ""))
    status = "✗ 失败" if is_err else "完成"
    line = f"  >> {name} {summary} → {status}"
    if is_err and text:
        err_key = text.split("\n")[0][:200] if text else text[:200]
        line += f"\n     错误: {err_key}"
    elif text and len(text) < 300:
        line += f"\n     输出: {text[:280]}"
    return line


def main():
    parser = argparse.ArgumentParser(description="从 threads.db 提取指定日期的对话")
    parser.add_argument("date", nargs="?", help="日期，格式 YYYY-MM-DD")
    parser.add_argument("--db", default=get_db_path(), help="SQLite 数据库路径")
    parser.add_argument("--out", default=None, help="输出文件路径 (默认 /tmp/learn-day-YYYY-MM-DD.txt)")
    parser.add_argument("--split", action="store_true", help="按 thread 拆分输出到目录")
    parser.add_argument("--cwd", default=None, help="项目目录过滤（仅提取 cwd 以此路径开头的 thread）")
    parser.add_argument("--all", action="store_true", help="不过滤项目目录（默认行为，提取所有项目）")
    parser.add_argument("--query-active-days", action="store_true", help="查询最近 N 天的活跃日期列表（不提取内容）")
    parser.add_argument("--days", type=int, default=7, help="配合 --query-active-days 的自然日期数量，含今天（默认 7）")
    args = parser.parse_args()

    if not os.path.exists(args.db):
        print(f"错误: 数据库文件不存在: {args.db}", file=sys.stderr)
        sys.exit(1)

    # --query-active-days 独立模式
    if args.query_active_days:
        rows = query_active_days(args.db, days=args.days, cwd=args.cwd)
        if not rows:
            print(f"最近 {args.days} 个自然日期无活跃 thread" + (f"（项目: {args.cwd}）" if args.cwd else ""))
        else:
            print(f"{'Day':>12}  {'Threads':>8}  {'Msgs':>8}")
            print("-" * 32)
            for r in rows:
                print(f"{r['day']:>12}  {r['thread_count']:>8}  {r['total_msgs']:>8}")
            total_threads = sum(r['thread_count'] for r in rows)
            total_msgs = sum(r['total_msgs'] for r in rows)
            print("-" * 32)
            print(f"{'TOTAL':>12}  {total_threads:>8}  {total_msgs:>8}")
        return

    if not args.date:
        parser.error("必须指定日期，或使用 --query-active-days")

    if args.split:
        out_dir = args.out or f"/tmp/learn-{args.date}"
        count, results = extract_date_by_thread(args.date, args.db, out_dir, cwd=args.cwd)
        print(f"提取完成: {args.date} → {out_dir}/")
        print(f"线程数: {count}")
        total_kb = sum(r["size_kb"] for r in results.values())
        print(f"总大小: {total_kb:.0f} KB ({len(results)} files)")
        # 显示多项目统计
        cwds = set(r.get("cwd", "?") for r in results.values())
        if len(cwds) > 1:
            print(f"多项目目录:")
            for c in sorted(cwds):
                count_c = sum(1 for r in results.values() if r.get("cwd") == c)
                print(f"  {c} ({count_c} threads)")
    else:
        out_path = args.out or f"/tmp/learn-day-{args.date}.txt"
        count, _ = extract_date(args.date, args.db, out_path, cwd=args.cwd)
        print(f"提取完成: {args.date} → {out_path}")
        print(f"线程数: {count}")
        print(f"文件大小: {os.path.getsize(out_path)} 字节")


if __name__ == "__main__":
    main()
