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


def _tool_call(name, tid, inp, stats):
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
    return {"id": tid, "name": redact_sensitive(name), "summary": param_summary}


def _valid_persisted_id(value):
    """UUID string forms accepted by MessageId's UUID serde representation."""
    if not isinstance(value, str):
        return False
    hyphenated = r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}"
    return re.fullmatch(rf"(?:[0-9a-fA-F]{{32}}|{hyphenated}|\{{{hyphenated}\}}|urn:uuid:{hyphenated})", value) is not None


def _valid_system_reminder(reminder):
    """Validate the persisted V1 DTO against system_reminder.rs; do not confer trust."""
    if not isinstance(reminder, dict):
        return False
    if (type(reminder.get("version")) is not int or reminder["version"] != 1
            or not all(isinstance(reminder.get(key), str) for key in ("source", "kind", "body"))
            or any(re.fullmatch(r"[a-z][a-z0-9_]{0,127}", reminder[key]) is None for key in ("source", "kind"))
            or reminder.get("category") not in ("capability", "task", "lifecycle", "guidance", "security", "external_event", "diagnostic", "legacy")
            or reminder.get("severity") not in ("info", "warning", "error", "critical")
            or reminder.get("delivery") not in ("required", "configurable", "diagnostic_only")):
        return False
    audiences = reminder.get("audiences")
    if (not isinstance(audiences, list) or not audiences
            or any(a not in ("model", "tui", "diagnostics", "automation") for a in audiences)
            or len(set(audiences)) != len(audiences)):
        return False
    summary = reminder.get("summary")
    metadata = reminder.get("metadata", {})
    if (summary is not None and not isinstance(summary, str)) or not isinstance(metadata, dict):
        return False
    try:
        if (len(reminder["body"].encode("utf-8")) > 64 * 1024
                or (summary is not None and len(summary.encode("utf-8")) > 4 * 1024)
                or len(json.dumps(reminder, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode("utf-8")) > 96 * 1024):
            return False
        # Match the Rust validator's node/depth and approximate serialized-byte budget.
        byte_count = node_count = 0
        stack = [(metadata, 1)]
        while stack:
            value, depth = stack.pop()
            node_count += 1
            if node_count > 1024 or depth > 16:
                return False
            if isinstance(value, dict):
                byte_count += len(value) + sum(len(key.encode("utf-8")) for key in value)
                stack.extend((child, depth + 1) for child in value.values())
            elif isinstance(value, list):
                byte_count += len(value)
                stack.extend((child, depth + 1) for child in value)
            elif isinstance(value, str):
                byte_count += len(value.encode("utf-8"))
            else:
                byte_count += len(json.dumps(value, allow_nan=False))
            if byte_count > 16 * 1024:
                return False
    except (UnicodeError, ValueError):
        return False
    return True


def _parse_message_details(row):
    """Read legacy BaseMessage or the store's V1 envelope, without projecting reasoning."""
    msg_id, role, raw = row
    stats = {"truncations": 0, "parse_failures": 0}
    parsed = {"id": msg_id, "role": role, "text": "", "tool_calls": [],
              "tool_results": [], "is_error": False, "stats": stats}

    def failed():
        stats["parse_failures"] += 1
        return PARSE_FAILURE_MARKER

    def bounded(text, limit, label):
        text, truncated = truncate_text(text, limit, label)
        stats["truncations"] += int(truncated)
        return text

    def invalid_payload():
        parsed["text"] = failed()
        return parsed

    try:
        payload = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return invalid_payload()

    # Version/type select the envelope just as deserialize_persisted_payload does.
    enveloped = isinstance(payload, dict) and ("version" in payload or "type" in payload)
    if enveloped:
        if type(payload.get("version")) is not int or payload["version"] != 1:
            return invalid_payload()
        if payload.get("type") == "system_reminder":
            parsed["role"] = "system_reminder"
            reminder = payload.get("reminder")
            if not _valid_persisted_id(payload.get("id")) or not _valid_system_reminder(reminder):
                return invalid_payload()
            provenance = bounded(f"source={reminder['source']} kind={reminder['kind']}", 400, "reminder source")
            body = bounded(reminder["body"], MAX_TOOL_RESULT_CHARS, "system reminder")
            parsed["text"] = f"{provenance}\n{body}"
            return parsed
        if payload.get("type") != "message" or not isinstance(payload.get("message"), dict):
            return invalid_payload()
        payload = payload["message"]
        if (not _valid_persisted_id(payload.get("id"))
                or payload.get("role") not in ("user", "assistant", "system", "tool")):
            return invalid_payload()

    text_limit = MAX_TOOL_RESULT_CHARS if role == "tool" else MAX_PLAIN_TEXT_CHARS
    if isinstance(payload, str):
        parsed["text"] = bounded(payload, text_limit, "plain message")
        return parsed
    if not isinstance(payload, dict) or "content" not in payload:
        return invalid_payload()
    if payload.get("role", role) != role or role not in ("user", "assistant", "system", "tool"):
        return invalid_payload()

    text_parts = []
    calls_by_id = {}

    def add_call(block, input_key):
        if (not isinstance(block, dict) or not isinstance(block.get("id"), str) or not block["id"]
                or not isinstance(block.get("name"), str) or not block["name"]
                or input_key not in block):
            text_parts.append(failed())
            return
        # ContentBlock::ToolUse is canonical; top-level tool_calls is a derived cache.
        if block["id"] not in calls_by_id:
            calls_by_id[block["id"]] = _tool_call(block["name"], block["id"], block[input_key], stats)

    def content_text(value, allow_tools=True):
        if isinstance(value, str):
            return value
        if not isinstance(value, list):
            return failed()
        parts = []
        for block in value:
            if not isinstance(block, dict):
                parts.append(failed())
                continue
            kind = block.get("type")
            if kind in ("reasoning", "thinking", "redacted_thinking"):
                continue
            if kind == "text":
                parts.append(block["text"] if isinstance(block.get("text"), str) else failed())
            elif kind in ("image", "document"):
                parts.append(f"[NON_TEXT_OMITTED: {kind}]")
            elif kind == "tool_use" and allow_tools:
                add_call(block, "input")
            elif kind == "tool_result" and allow_tools:
                parse_result(block.get("tool_use_id"), block.get("content"), block.get("is_error", False))
            else:
                parts.append(failed())
        return "\n".join(parts)

    def parse_result(call_id, content, is_error):
        before = stats["parse_failures"]
        result_text = content_text(content, allow_tools=False)
        if not isinstance(call_id, str) or not call_id:
            result_text += "\n" + failed()
            call_id = ""
        if type(is_error) is not bool:
            result_text += "\n" + failed()
            is_error = False
        parsed["tool_results"].append({
            "id": call_id, "text": bounded(result_text, MAX_TOOL_RESULT_CHARS, "tool result"),
            "is_error": is_error, "received": True, "valid": stats["parse_failures"] == before,
        })
        parsed["is_error"] = parsed["is_error"] or is_error

    if role == "tool":
        parse_result(payload.get("tool_call_id"), payload["content"], payload.get("is_error", False))
        parsed["text"] = parsed["tool_results"][0]["text"]
    else:
        text_parts.insert(0, content_text(payload["content"]))
        calls = payload.get("tool_calls", [])
        if not isinstance(calls, list):
            text_parts.append(failed())
        else:
            for call in calls:
                add_call(call, "arguments")
        limit = MAX_PLAIN_TEXT_CHARS if isinstance(payload["content"], str) else MAX_MESSAGE_TEXT_CHARS
        parsed["text"] = bounded("\n".join(part for part in text_parts if part), limit, "message text")
    parsed["tool_calls"] = list(calls_by_id.values())
    return parsed


def parse_message(row):
    """Public compatibility tuple, including explicit truncation/parse-failure counts."""
    parsed = _parse_message_details(row)
    return (parsed["id"], parsed["role"], parsed["text"], parsed["tool_calls"],
            parsed["is_error"], parsed["stats"])


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

    # Keep call entries in original order; later results update only the matching ID.
    ordered = []
    pending_calls = {}
    truncation_count = 0
    parse_failure_count = 0
    for msg in messages:
        parsed = _parse_message_details(msg)
        stats = parsed["stats"]
        truncation_count += stats["truncations"]
        parse_failure_count += stats["parse_failures"]
        role, text = parsed["role"], parsed["text"]
        if text and (role != "tool" or not parsed["tool_results"]):
            ordered.append({"type": role + "_text", "text": text})
        for call in parsed["tool_calls"]:
            entry = {"type": "tool_call", **call, "is_error": False,
                     "text": "", "received": False, "valid": False}
            ordered.append(entry)
            pending_calls.setdefault(call["id"], []).append(entry)
        for result in parsed["tool_results"]:
            matching = pending_calls.get(result["id"], [])
            if matching:
                matching.pop(0).update(result)
            else:
                # Orphan results are evidence too; never silently drop their failures.
                ordered.append({"type": "tool_call", "name": "[未匹配工具结果]",
                                "summary": redact_sensitive(result["id"]), **result})

    # Merge only identical observations. A retry must not erase a preceding error.
    deduped = []
    error_count = sum(entry.get("is_error", False) for entry in ordered)
    for entry in ordered:
        if (deduped and entry["type"] == "tool_call"
                and deduped[-1]["type"] in ("tool_call", "dup_tool")
                and all(deduped[-1].get(key) == entry.get(key)
                        for key in ("name", "summary", "is_error", "text", "received", "valid"))):
            deduped[-1]["type"] = "dup_tool"
            deduped[-1]["count"] = deduped[-1].get("count", 1) + 1
        else:
            deduped.append(entry.copy())

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

        elif entry["type"] in ("system_reminder_text", "system_text", "tool_text"):
            label = {"system_reminder_text": "系统提醒", "system_text": "系统消息", "tool_text": "工具消息"}[entry["type"]]
            lines.append(f"[{label}]:")
            lines.extend(f"  {line}" for line in entry["text"].split("\n"))
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
    if is_err:
        status = "✗ 失败"
    elif not entry.get("received"):
        status = "未收到结果"
    elif not entry.get("valid"):
        status = "结果无法解析"
    else:
        status = "完成"
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
