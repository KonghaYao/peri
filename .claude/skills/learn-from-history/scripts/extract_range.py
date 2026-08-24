#!/usr/bin/env python3
"""
批量提取指定时间段内的对话历史，转为精简纯文本。

默认模式（推荐）：按 thread 拆分，每天一个目录，每个 thread 一个文件。
  单个文件通常 <50KB，agent 一次 Read 就能完整读取。

用法:
  # 默认：按 thread 拆分（推荐），自动按当前 cwd 过滤项目
  python3 extract_range.py 2026-06-12 2026-06-19

  # 查询活跃天数（不提取内容，替代直接 SQL 查询）
  python3 extract_range.py --query-active-days

  # 指定项目目录过滤
  python3 extract_range.py 2026-06-12 2026-06-19 --cwd /path/to/project

  # 不过滤项目（提取所有项目）
  python3 extract_range.py 2026-06-12 2026-06-19 --all

  # 指定输出根目录
  python3 extract_range.py 2026-06-12 2026-06-19 --out-root /tmp

  # 按天合并（旧行为，一个大文件）
  python3 extract_range.py 2026-06-12 2026-06-19 --merge

  # 时间段合并为单个大文件
  python3 extract_range.py 2026-06-12 2026-06-19 --merge --out /tmp/all.txt

输出目录结构（默认模式）：
  /tmp/learn-2026-06-15/
    _index.txt                          # 索引：文件名 | 消息数 | 错误数 | 大小 | 标题
    019ec8ee-7f0c.txt                   # ~14KB
    019ec8df-b700.txt                   # ~21KB
    019ec948-18KB.txt                   # ~18KB
    ...

底层复用 extract_daily.py 的提取逻辑。
"""

import sys
import os
import argparse
from datetime import datetime, timedelta

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from extract_daily import extract_date, extract_date_by_thread, get_db_path, query_active_days


def parse_date_range():
    parser = argparse.ArgumentParser(
        description="批量提取指定时间段的对话历史（精简纯文本）"
    )
    parser.add_argument(
        "dates", nargs="*",
        help="起始日期和结束日期，格式 YYYY-MM-DD YYYY-MM-DD"
    )
    parser.add_argument("--from", dest="from_date", help="起始日期 YYYY-MM-DD")
    parser.add_argument("--to", dest="to_date", help="结束日期 YYYY-MM-DD")
    parser.add_argument("--db", default=get_db_path(), help="SQLite 数据库路径")
    parser.add_argument(
        "--out-root", default="/tmp",
        help="按 thread 拆分时的输出根目录（默认 /tmp）"
    )
    parser.add_argument(
        "--merge", action="store_true",
        help="按天合并模式（旧行为：每天一个大文件）"
    )
    parser.add_argument(
        "--out", default=None,
        help="配合 --merge：输出单个合并文件"
    )
    parser.add_argument(
        "--cwd", default=None,
        help="项目目录过滤（只提取该项目或其子目录的 thread）。默认：当前工作目录"
    )
    parser.add_argument(
        "--all", action="store_true",
        help="不过滤项目目录（提取所有项目的 thread）"
    )
    parser.add_argument(
        "--query-active-days", action="store_true",
        help="查询最近 N 天的活跃日期列表（不提取内容，替代 SQL）"
    )
    parser.add_argument(
        "--days", type=int, default=7,
        help="配合 --query-active-days 的自然日期数量，含今天（默认 7）"
    )

    args = parser.parse_args()

    # --query-active-days 模式不需要日期参数
    if args.query_active_days:
        return args, None, None

    if args.from_date and args.to_date:
        start_str, end_str = args.from_date, args.to_date
    elif len(args.dates) >= 2:
        start_str, end_str = args.dates[0], args.dates[1]
    elif len(args.dates) == 1:
        start_str = end_str = args.dates[0]
    else:
        parser.print_help()
        sys.exit(1)

    return args, start_str, end_str


def iter_dates(start_str, end_str):
    start = datetime.fromisoformat(start_str)
    end = datetime.fromisoformat(end_str)
    current = start
    while current <= end:
        yield current.strftime("%Y-%m-%d")
        current += timedelta(days=1)


def main():
    args, start_str, end_str = parse_date_range()

    if not os.path.exists(args.db):
        print(f"错误: 数据库文件不存在: {args.db}", file=sys.stderr)
        sys.exit(1)

    # --query-active-days：查询活跃日期（不提取内容）
    if args.query_active_days:
        cwd = _resolve_cwd(args)
        rows = query_active_days(args.db, days=args.days, cwd=cwd)
        if not rows:
            print(f"最近 {args.days} 个自然日期无活跃 thread" + (f"（项目: {cwd}）" if cwd else "（所有项目）"))
            sys.exit(0)
        print(f"{'Day':>12}  {'Threads':>8}  {'Msgs':>8}")
        print("-" * 32)
        for r in rows:
            print(f"{r['day']:>12}  {r['thread_count']:>8}  {r['total_msgs']:>8}")
        total_threads = sum(r['thread_count'] for r in rows)
        total_msgs = sum(r['total_msgs'] for r in rows)
        print("-" * 32)
        print(f"{'TOTAL':>12}  {total_threads:>8}  {total_msgs:>8}")
        if cwd:
            print(f"\n过滤项目: {cwd}")
        return 0

    dates = list(iter_dates(start_str, end_str))
    cwd = _resolve_cwd(args)

    if cwd:
        print(f"项目过滤: {cwd}")
    print(f"时间段: {start_str} ~ {end_str}，共 {len(dates)} 天")
    print(f"数据库: {args.db}\n")

    total_threads = 0
    has_any_data = False

    if args.merge:
        # 按天合并模式（旧行为）
        return _run_merge_mode(args, dates, cwd)

    # 默认：按 thread 拆分
    failed_dates = []
    for day in dates:
        out_dir = os.path.join(args.out_root, f"learn-{day}")
        try:
            count, results = extract_date_by_thread(day, args.db, out_dir, cwd=cwd)
            if count > 0:
                total_size = sum(r["size_kb"] for r in results.values())
                total_errors = sum(r["errors"] for r in results.values())
                cwds = set(r.get("cwd", "?") for r in results.values())
                cwd_info = f" [{len(cwds)} projects]" if len(cwds) > 1 else ""
                print(f"  ✓ {day}: {count} threads{cwd_info}, {total_size:.0f} KB, {total_errors} errors → {out_dir}/")
                total_threads += count
                has_any_data = True
            else:
                print(f"  - {day}: 无活跃线程")
        except Exception as error:
            failed_dates.append(day)
            print(f"  ✗ {day}: 提取失败 - {error}", file=sys.stderr)

    if has_any_data:
        print(f"\n总计: {total_threads} threads")
        if cwd:
            print(f"过滤项目: {cwd}")
        print()
        print("供手工导出的目录列表:")
        for day in sorted(dates):
            out_dir = os.path.join(args.out_root, f"learn-{day}")
            idx_path = os.path.join(out_dir, "_index.txt")
            if os.path.exists(idx_path):
                thread_count = sum(1 for filename in os.listdir(out_dir) if filename.endswith(".txt") and filename != "_index.txt")
                if thread_count > 0:
                    print(f"  {out_dir}/  ({thread_count} threads)")
    else:
        msg = "时间段内无对话记录。"
        if cwd:
            msg += " 尝试 --all 查看所有项目。"
        print(msg)

    if failed_dates:
        print(f"提取失败日期: {', '.join(failed_dates)}", file=sys.stderr)
        return 1
    return 0


def _resolve_cwd(args):
    """解析 cwd 过滤参数。优先级：--all（null） > --cwd（指定） > 当前 cwd

    调用方（agent）应始终从 env 中获取工作目录，显式传递 --cwd。
    """
    if args.all:
        return None
    if args.cwd is not None:
        return args.cwd
    return os.getcwd()


def _run_merge_mode(args, dates, cwd=None):
    """按天合并模式（旧行为，保留向后兼容）"""
    has_any = False

    if args.out:
        merge_file = args.out
        out_dir = os.path.dirname(merge_file) or "."
        os.makedirs(out_dir, exist_ok=True)
        print(f"模式: 合并输出 → {merge_file}\n")
    else:
        out_dir = args.out_root
        os.makedirs(out_dir, exist_ok=True)
        print(f"模式: 按天合并 → {out_dir}/learn-day-YYYY-MM-DD.txt\n")

    day_results = {}
    failed_dates = []

    for day in dates:
        output_path = os.path.join(out_dir, f"learn-day-{day}.txt")
        try:
            count, _ = extract_date(day, args.db, output_path, cwd=cwd)
            if count > 0:
                size_kb = os.path.getsize(output_path) / 1024
                size_warn = " ⚠️ 大文件" if size_kb > 200 else ""
                print(f"  ✓ {day}: {count} threads, {size_kb:.0f} KB{size_warn}")
                day_results[day] = {"path": output_path, "size_kb": size_kb}
                has_any = True
            else:
                print(f"  - {day}: 无活跃线程")
        except Exception as error:
            failed_dates.append(day)
            print(f"  ✗ {day}: 提取失败 - {error}", file=sys.stderr)

    if not has_any:
        print("\n时间段内无对话记录。")
    elif args.out and day_results:
        print(f"\n合并 {len(day_results)} 天数据到 {merge_file} ...")
        fd = os.open(merge_file, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        os.fchmod(fd, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as out_f:
            for day in sorted(day_results.keys()):
                with open(day_results[day]["path"], "r", encoding="utf-8") as in_f:
                    content = in_f.read()
                    lines = content.split("\n")
                    out_f.write(f"\n{'='*60}\n# 日期: {day}\n{'='*60}\n\n")
                    start = 3 if lines[0].startswith("# 对话历史提取") else 0
                    out_f.write("\n".join(lines[start:]))
                    out_f.write("\n")
        os.chmod(merge_file, 0o600)
        print(f"合并完成: {os.path.getsize(merge_file) / (1024*1024):.1f} MB")

    if failed_dates:
        print(f"提取失败日期: {', '.join(failed_dates)}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
