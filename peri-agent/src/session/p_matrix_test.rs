//! P02 / P06 / P07 原生测量：连续 Full、释放屏障、历史正文规模曲线。
//!
//! 这些用例是**测量**而非阈值回归：
//!
//! - 全部使用真实临时 SQLite（[`TestSession`]）与真实
//!   `commit_compaction_lifecycle`，不调用外部模型；
//! - 逻辑驻留来自 [`MessageTranscript::memory_snapshot`]（String/Vec 的使用量与
//!   capacity 下界），不是分配器统计；
//! - allocator / RSS 只作辅助观察，单独不证明正文泄漏，也不承诺固定降幅；
//! - 平台接口：macOS 用 libc 的 `malloc_zone_statistics` + `proc_pid_rusage`；
//!   Linux 读 `/proc/self/status`（无 allocator in-use）；其它平台只报告不可用。
//!
//! 运行方式（`--ignored`）：
//!
//! ```text
//! ./scripts/cargo-rmcp-patched.sh test --locked -p peri-agent --lib \
//!   -- --ignored --nocapture p02_
//! PERI_P07_MIB=512 ./scripts/... -- --ignored --nocapture p07_
//! ```

use super::*;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use peri_acp_types::store::{CompactionChange, MessageFlags, PersistedPayload};

use crate::messages::MessageId;
use crate::session::memory_probe::message_bytes;
use crate::session::test_resources::TestSession;

// ─── 夹具口径 ────────────────────────────────────────────────────────────────

const INHERITED_MESSAGES: usize = 2;
const SUMMARY_TEXT_BYTES: usize = 512;
const P02_WORK_PAIRS: usize = 6;
const P02_WORK_BODY_BYTES: usize = 16 * 1024;
const P02_ROUNDS: usize = 10;
const P06_INITIAL_PAIRS: usize = 60;
const P06_BODY_BYTES: usize = 8 * 1024;
const P06_CONTINUED_ROUNDS: usize = 4;

fn make_human_body(prefix: &str, body_bytes: usize) -> BaseMessage {
    make_human(&format!("{prefix}-{}", "h".repeat(body_bytes)))
}

fn make_ai_body(prefix: &str, body_bytes: usize) -> BaseMessage {
    make_ai(&format!("{prefix}-{}", "a".repeat(body_bytes)))
}

fn make_summary(round: usize) -> BaseMessage {
    make_ai(&format!(
        "FULL-SUMMARY-{round:02}-{}",
        "s".repeat(SUMMARY_TEXT_BYTES)
    ))
}

fn summary_body_bytes() -> usize {
    message_bytes(&make_summary(0)).body
}

fn own_flag_updates(
    transcript: &MessageTranscript,
    ancestor_len: usize,
) -> Vec<(MessageId, MessageFlags)> {
    transcript
        .entries()
        .iter()
        .skip(ancestor_len)
        .map(|entry| {
            (
                entry.id(),
                MessageFlags {
                    excluded: true,
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn assert_ids_absent(transcript: &MessageTranscript, ids: &[MessageId]) {
    for id in ids {
        assert!(
            transcript.get(*id).is_none(),
            "归档条目 {id:?} 必须离开热态 entries/id_index"
        );
    }
    let remaining: HashSet<MessageId> = transcript
        .entries()
        .iter()
        .map(|entry| entry.id())
        .collect();
    for id in ids {
        assert!(!remaining.contains(id), "归档条目 {id:?} 仍被热态容器引用");
    }
}

fn active_text(transcript: &MessageTranscript) -> String {
    transcript
        .active_payloads()
        .iter()
        .filter_map(|payload| payload.as_message())
        .map(|message| message.content())
        .collect::<Vec<_>>()
        .join("\n")
}

fn mib(value: u64) -> f64 {
    value as f64 / (1024.0 * 1024.0)
}

fn fmt_mib(value: Option<u64>) -> String {
    value
        .map(|value| format!("{:.1}MiB", mib(value)))
        .unwrap_or_else(|| "n/a".into())
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

// ─── 进程级辅助观察（allocator / RSS） ───────────────────────────────────────

#[derive(Clone, Copy, Debug, Default)]
struct ProcessMemory {
    rss_bytes: Option<u64>,
    phys_footprint_bytes: Option<u64>,
    malloc_size_in_use: Option<u64>,
    malloc_blocks_in_use: Option<u64>,
    platform: &'static str,
}

/// 进程 RSS（与 TUI 采样同源：sysinfo）；平台接口不可用时仍可给出 OS 级 RSS。
#[cfg(not(target_os = "windows"))]
fn sysinfo_rss_bytes() -> Option<u64> {
    use sysinfo::{ProcessesToUpdate, System};
    let mut system = System::new();
    let pid = sysinfo::get_current_pid().ok()?;
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
    system.process(pid).map(sysinfo::Process::memory)
}

#[cfg(target_os = "windows")]
fn sysinfo_rss_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
fn sample_process_memory() -> ProcessMemory {
    // SAFETY: 只读系统统计；两个 API 都按官方签名填充调用方提供的结构体。
    unsafe {
        let zone = libc::malloc_default_zone();
        let mut stats: libc::malloc_statistics_t = std::mem::zeroed();
        libc::malloc_zone_statistics(zone, &mut stats);

        let mut usage: libc::rusage_info_v4 = std::mem::zeroed();
        let pid = libc::getpid();
        let mut usage_ptr: libc::rusage_info_t =
            &mut usage as *mut libc::rusage_info_v4 as libc::rusage_info_t;
        let rc = libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V4, &mut usage_ptr);
        let (rss, footprint) = if rc == 0 {
            let rss = (usage.ri_resident_size > 0).then_some(usage.ri_resident_size);
            let footprint = (usage.ri_phys_footprint > 0).then_some(usage.ri_phys_footprint);
            (rss, footprint)
        } else {
            (None, None)
        };
        ProcessMemory {
            rss_bytes: rss.or_else(sysinfo_rss_bytes),
            phys_footprint_bytes: footprint,
            malloc_size_in_use: Some(stats.size_in_use as u64),
            malloc_blocks_in_use: Some(stats.blocks_in_use as u64),
            platform: "macos: malloc_zone_statistics + proc_pid_rusage(V4) + sysinfo RSS",
        }
    }
}

#[cfg(target_os = "linux")]
fn sample_process_memory() -> ProcessMemory {
    fn field(status: &str, key: &str) -> Option<u64> {
        status.lines().find_map(|line| {
            let rest = line.strip_prefix(key)?.trim_start();
            let value = rest.split_whitespace().next()?;
            value.parse::<u64>().ok().map(|kib| kib * 1024)
        })
    }
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    ProcessMemory {
        rss_bytes: field(&status, "VmRSS:"),
        phys_footprint_bytes: field(&status, "VmHWM:"),
        malloc_size_in_use: None,
        malloc_blocks_in_use: None,
        platform: "linux: /proc/self/status (allocator in-use unavailable)",
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn sample_process_memory() -> ProcessMemory {
    ProcessMemory {
        platform: "unsupported: no allocator/RSS sampler",
        ..Default::default()
    }
}

fn print_memory(stage: &str, memory: &ProcessMemory) {
    println!(
        "  {stage} platform={} rss={} footprint={} malloc_in_use={} malloc_blocks={}",
        memory.platform,
        fmt_mib(memory.rss_bytes),
        fmt_mib(memory.phys_footprint_bytes),
        fmt_mib(memory.malloc_size_in_use),
        memory
            .malloc_blocks_in_use
            .map(|v| v.to_string())
            .unwrap_or_else(|| "n/a".into()),
    );
}

fn print_transcript(
    stage: &str,
    snapshot: &crate::session::memory_probe::TranscriptMemorySnapshot,
) {
    println!(
        "  {stage} own_entries={} inherited_entries={} active={} excluded={} \
         own_body={}B own_reserved={}B entries_cap={} entries_cap_bytes={} \
         id_index_cap={} id_index_cap_bytes={} flags_cap={} flags_cap_bytes={} staged={}B",
        snapshot.own_entries,
        snapshot.inherited_entries,
        snapshot.active_entries,
        snapshot.excluded_entries,
        snapshot.own_bytes.body,
        snapshot.own_bytes.reserved,
        snapshot.entries_capacity,
        snapshot.entries_capacity_bytes,
        snapshot.id_index_capacity,
        snapshot.id_index_capacity_bytes,
        snapshot.flags_capacity,
        snapshot.flags_capacity_bytes,
        snapshot.staged_bytes.body,
    );
}

// ─── P02：至少 10 次 Full，任务中途不退出 ────────────────────────────────────

#[derive(Clone, Copy)]
struct P02Round {
    round: usize,
    t0: crate::session::memory_probe::TranscriptMemorySnapshot,
    t1: crate::session::memory_probe::TranscriptMemorySnapshot,
    commit_elapsed: Duration,
}

/// P02：同一 transcript（任务不退出）连续 10 次 Full。
///
/// 每轮：追加固定工作负载 → T0 → Full（排除全部自有条目 + 追加摘要）→ T1 →
/// 继续追加下一轮工作。判据是 T1/T0 的**条目、正文与容器容量**不随轮次增长，
/// 且上一轮归档正文不再能被热态导出；SQLite 中历史仍完整保留。
#[tokio::test]
#[ignore = "P 矩阵测量（10 次连续 Full，真实 SQLite）；运行方式见模块文档"]
async fn p02_ten_full_compactions_keep_hot_baseline_bounded() {
    let session = TestSession::open().await;
    let ancestor_len = INHERITED_MESSAGES;
    let inherited: Vec<PersistedPayload> = (0..INHERITED_MESSAGES)
        .map(|index| {
            PersistedPayload::Message(make_human(&format!(
                "inherited-{index}-{}",
                "i".repeat(128)
            )))
        })
        .collect();
    let mut transcript = MessageTranscript::new()
        .with_persistence(session.resources(), session.thread_id.clone())
        .with_ancestor_payloads(inherited);

    let summary_body = summary_body_bytes();
    let work_body_per_round: usize = (0..P02_WORK_PAIRS)
        .map(|pair| {
            message_bytes(&make_human_body(
                &format!("P02-R00-H{pair:02}"),
                P02_WORK_BODY_BYTES,
            ))
            .body
                + message_bytes(&make_ai_body(
                    &format!("P02-R00-A{pair:02}"),
                    P02_WORK_BODY_BYTES,
                ))
                .body
        })
        .sum();
    let started = Instant::now();
    let mut rounds: Vec<P02Round> = Vec::with_capacity(P02_ROUNDS);
    let mut archived_by_round: Vec<Vec<MessageId>> = Vec::new();

    println!("== P02 环境 ==");
    print_memory("process", &sample_process_memory());
    println!(
        "  夹具：inherited={INHERITED_MESSAGES}；每轮工作 {P02_WORK_PAIRS} 对 × {P02_WORK_BODY_BYTES}B；\
         摘要正文 {summary_body}B；rounds={P02_ROUNDS}"
    );

    for round in 0..P02_ROUNDS {
        // 任务继续工作：固定大小的一轮 ReAct 产出。
        for pair in 0..P02_WORK_PAIRS {
            transcript.append(make_human_body(
                &format!("P02-R{round:02}-H{pair:02}"),
                P02_WORK_BODY_BYTES,
            ));
            transcript.append(make_ai_body(
                &format!("P02-R{round:02}-A{pair:02}"),
                P02_WORK_BODY_BYTES,
            ));
        }
        transcript.flush_persistence().await.unwrap();

        let t0 = transcript.memory_snapshot();
        let flagged = own_flag_updates(&transcript, ancestor_len);
        let archived_ids: Vec<MessageId> = flagged.iter().map(|(id, _)| *id).collect();
        assert_eq!(
            archived_ids.len(),
            P02_WORK_PAIRS * 2 + usize::from(round > 0),
            "本轮归档集合必须覆盖上一轮摘要（round > 0）与本轮工作负载"
        );

        let summary = make_summary(round);
        let summary_id = summary.id();
        let commit_started = Instant::now();
        transcript
            .commit_compaction_lifecycle(CompactionChange {
                flag_updates: flagged,
                appended_messages: vec![summary],
            })
            .await
            .unwrap();
        let commit_elapsed = commit_started.elapsed();

        let t1 = transcript.memory_snapshot();

        // 归档条目确实离开热态；有效集合只剩本轮摘要。
        assert_ids_absent(&transcript, &archived_ids);
        assert!(transcript.get(summary_id).is_some());
        assert_eq!(t1.own_entries, 1, "round {round}: T1 只应保留本轮摘要");
        assert_eq!(
            t1.active_entries,
            INHERITED_MESSAGES + 1,
            "有效集合 = 冻结继承区 + 本轮摘要"
        );
        assert_eq!(t1.excluded_entries, 0, "收缩后不得残留 excluded 热态条目");
        assert_eq!(t1.flags_capacity, 0, "被释放条目的 flags 不得继续驻留");
        assert_eq!(t1.own_bytes.body, summary_body);
        assert_eq!(
            t1.inherited_entries, INHERITED_MESSAGES,
            "冻结继承区不得被 Full 改动"
        );
        let text = active_text(&transcript);
        assert!(text.contains(&format!("FULL-SUMMARY-{round:02}-")));
        assert!(!text.contains("P02-R"), "归档工作正文不得出现在有效集合");

        // 每轮 T0 是「上一轮摘要（round > 0）+ 本轮固定工作」，不随轮次累计。
        let expected_t0_entries = P02_WORK_PAIRS * 2 + usize::from(round > 0);
        let expected_t0_body = work_body_per_round + if round > 0 { summary_body } else { 0 };
        assert_eq!(
            t0.own_entries, expected_t0_entries,
            "round {round}: T0 必须只有固定工作集"
        );
        assert_eq!(t0.own_bytes.body, expected_t0_body);
        rounds.push(P02Round {
            round,
            t0,
            t1,
            commit_elapsed,
        });
        archived_by_round.push(archived_ids);
    }

    println!("== P02 每轮对象计量 ==");
    for row in &rounds {
        println!(
            "round={:02} commit_ms={:.3}",
            row.round + 1,
            row.commit_elapsed.as_secs_f64() * 1000.0
        );
        print_transcript("T0", &row.t0);
        print_transcript("T1", &row.t1);
        let memory = sample_process_memory();
        print_memory("process(T1)", &memory);
    }

    // 基线稳定：第 1 轮起（已有上一轮摘要）T0/T1 完全一致（正文长度固定）。
    let first = rounds.first().expect("至少一轮");
    let baseline = &rounds[1];
    for row in &rounds[2..] {
        assert_eq!(row.t0.own_entries, baseline.t0.own_entries);
        assert_eq!(row.t0.own_bytes.body, baseline.t0.own_bytes.body);
        assert_eq!(row.t1.own_entries, first.t1.own_entries);
        assert_eq!(row.t1.own_bytes.body, first.t1.own_bytes.body);
        assert_eq!(row.t1.entries_capacity, first.t1.entries_capacity);
        assert_eq!(row.t1.id_index_capacity, first.t1.id_index_capacity);
    }
    let baseline_growth =
        rounds.last().unwrap().t0.own_bytes.body as i64 - baseline.t0.own_bytes.body as i64;
    assert_eq!(baseline_growth, 0, "T0 历史正文基线随 Full 次数增长");

    // 权威存储仍保留全部归档正文；热态释放不是删除历史。
    let stored = session
        .resources()
        .load_session_history(&session.thread_id)
        .await
        .unwrap();
    let expected = P02_ROUNDS * (P02_WORK_PAIRS * 2 + 1);
    assert_eq!(
        stored.len(),
        expected,
        "SQLite 中必须保留每轮工作负载与摘要（{expected} 条）"
    );
    let stored_text = stored
        .iter()
        .filter_map(|payload| payload.as_message())
        .map(|message| message.content())
        .collect::<Vec<_>>()
        .join("\n");
    for round in 0..P02_ROUNDS {
        assert!(
            stored_text.contains(&format!("P02-R{round:02}-H00")),
            "round {round} 的归档正文必须仍在 SQLite 中"
        );
    }
    for (round, ids) in archived_by_round.iter().enumerate() {
        let stored_ids: HashSet<MessageId> = stored.iter().map(|payload| payload.id()).collect();
        for id in ids {
            assert!(
                stored_ids.contains(id),
                "round {round}: 归档条目 {id:?} 必须仍在存储中"
            );
        }
    }

    transcript.shutdown_persistence();
    println!(
        "P02 汇总：rounds={P02_ROUNDS} 总耗时={:.3}s 提交耗时合计={:.3}s \
         T0 基线增长={}B stored_entries={} summary_body={}B",
        started.elapsed().as_secs_f64(),
        rounds
            .iter()
            .map(|row| row.commit_elapsed.as_secs_f64())
            .sum::<f64>(),
        baseline_growth,
        stored.len(),
        summary_body,
    );
}

// ─── P06：Full 后保持执行，最终结束 ──────────────────────────────────────────

/// P06：主要旧正文必须在**提交后的释放屏障**消失，而不是等任务结束。
///
/// 采样：T0（compact 前）→ T1（存储确认 + transcript 收缩，释放屏障）→
/// T2/T3/T4（继续 ReAct 工作）→ T5（任务结束）。对象计量逐阶段对比；
/// 归档正文到 T5 不得回流，T1 之后的正文变化只来自新工作集。
#[tokio::test]
#[ignore = "P 矩阵测量（Full 后继续执行与释放屏障，真实 SQLite）"]
async fn p06_full_release_barrier_is_independent_of_turn_end() {
    let session = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(session.resources(), session.thread_id.clone());
    let summary_body = summary_body_bytes();

    let mut archived_ids = Vec::new();
    for pair in 0..P06_INITIAL_PAIRS {
        archived_ids
            .push(transcript.append(make_human_body(&format!("P06-H{pair:02}"), P06_BODY_BYTES)));
        archived_ids
            .push(transcript.append(make_ai_body(&format!("P06-A{pair:02}"), P06_BODY_BYTES)));
    }
    transcript.flush_persistence().await.unwrap();

    let t0 = transcript.memory_snapshot();
    let mem_t0 = sample_process_memory();
    assert_eq!(t0.own_entries, P06_INITIAL_PAIRS * 2);
    assert!(
        t0.own_bytes.body >= P06_INITIAL_PAIRS * 2 * P06_BODY_BYTES,
        "夹具必须包含真实历史正文"
    );
    println!("== P06 环境 ==");
    print_memory("process", &mem_t0);
    println!(
        "  夹具：initial_pairs={P06_INITIAL_PAIRS} body={P06_BODY_BYTES}B 摘要={summary_body}B"
    );
    print_transcript("T0 pre-compact", &t0);

    let flagged = own_flag_updates(&transcript, 0);
    let summary = make_summary(0);
    let commit_started = Instant::now();
    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates: flagged,
            appended_messages: vec![summary],
        })
        .await
        .unwrap();
    let commit_elapsed = commit_started.elapsed();
    let t1 = transcript.memory_snapshot();
    print_transcript("T1 release barrier", &t1);
    print_memory("process(T1)", &sample_process_memory());

    assert_ids_absent(&transcript, &archived_ids);
    assert_eq!(t1.own_entries, 1);
    assert_eq!(t1.own_bytes.body, summary_body, "T1 必须只保留摘要正文");
    assert_eq!(t1.excluded_entries, 0);
    assert_eq!(t1.flags_capacity, 0);
    assert!(!active_text(&transcript).contains("P06-H"));

    // T2/T3：执行器/Host 交接与发布后旧请求排空。这里用一份只含有效集合的
    // handoff 快照模拟交接；它不得携带归档正文。
    let handoff = transcript.active_payloads();
    let handoff_bytes: usize = handoff
        .iter()
        .map(|payload| crate::session::memory_probe::payload_bytes(payload).body)
        .sum();
    assert_eq!(handoff.len(), 1);
    assert_eq!(handoff_bytes, summary_body);
    println!(
        "  T2 handoff_payloads={} handoff_body={}B（交接只携带有效集合）",
        handoff.len(),
        handoff_bytes
    );
    drop(handoff);
    transcript.flush_persistence().await.unwrap();
    let t3 = transcript.memory_snapshot();
    print_transcript("T3 drained", &t3);
    assert_eq!(t3.own_bytes.body, summary_body);

    // T4：长任务继续工作若干轮（每轮 human+ai 8 KiB）；归档正文不得回流。
    let mut continued_body = 0usize;
    for round in 0..P06_CONTINUED_ROUNDS {
        let human = make_human_body(&format!("P06-CONT-H{round}"), P06_BODY_BYTES);
        let ai = make_ai_body(&format!("P06-CONT-A{round}"), P06_BODY_BYTES);
        continued_body += message_bytes(&human).body + message_bytes(&ai).body;
        transcript.append(human);
        transcript.append(ai);
        transcript.flush_persistence().await.unwrap();
        let snapshot = transcript.memory_snapshot();
        assert_ids_absent(&transcript, &archived_ids);
        assert_eq!(
            snapshot.own_bytes.body,
            summary_body + continued_body,
            "round {round}: T4 只能包含摘要与新增工作集"
        );
        print_transcript(&format!("T4 round {}", round + 1), &snapshot);
        print_memory("process(T4)", &sample_process_memory());
    }

    // T5：任务结束；旧归档正文早已在 T1 释放，结束时不产生第二段大释放。
    let final_message = make_ai_body("P06-FINAL", P06_BODY_BYTES);
    let final_body = message_bytes(&final_message).body;
    transcript.append(final_message);
    transcript.flush_persistence().await.unwrap();
    let t5 = transcript.memory_snapshot();
    print_transcript("T5 task end", &t5);
    print_memory("process(T5)", &sample_process_memory());
    assert_ids_absent(&transcript, &archived_ids);
    assert_eq!(
        t5.own_bytes.body,
        summary_body + continued_body + final_body,
        "任务结束不得让归档正文回流"
    );

    let barrier_release = t0.own_bytes.body.saturating_sub(t1.own_bytes.body);
    let end_release = t1.own_bytes.body.saturating_sub(t5.own_bytes.body);
    let work_growth = t5.own_bytes.body.saturating_sub(t1.own_bytes.body);
    assert!(
        barrier_release >= t0.own_bytes.body - summary_body,
        "归档正文必须在 T1 屏障释放"
    );
    assert_eq!(end_release, 0, "T1 之后不应再有归档正文释放");
    println!(
        "P06 汇总：commit_ms={:.3} barrier_release={}B end_release={}B \
         work_growth={}B T0={}B T1={}B T5={}B",
        commit_elapsed.as_secs_f64() * 1000.0,
        barrier_release,
        end_release,
        work_growth,
        t0.own_bytes.body,
        t1.own_bytes.body,
        t5.own_bytes.body,
    );

    transcript.shutdown_persistence();
}

// ─── P07：64 MiB / 256 MiB / 512 MiB 历史正文曲线 ────────────────────────────

#[derive(Clone, Copy)]
struct P07Stage {
    name: &'static str,
    snapshot: crate::session::memory_probe::TranscriptMemorySnapshot,
    memory: ProcessMemory,
    elapsed_ms: f64,
}

/// P07：按 `PERI_P07_MIB`（默认 64）生成指定规模历史，记录 T0–T6。
///
/// T0：compact 前，整份历史在热 transcript；
/// T1：存储确认并完成 Transcript 收缩（释放屏障）；
/// T2：执行器/Host 交接完成——本进程用只含有效集合的 handoff 快照表示；
/// T3：handoff 与持久化队列排空；
/// T4：当前长任务继续工作若干轮；
/// T5：任务结束；
/// T6：反复读取归档页（存储级向更早翻页）后重新读取有效尾部。
///
/// 每个阶段同时记录逻辑驻留、allocator in-use 与 RSS/footprint。T6 的窗口级
/// 渲染缓存证据见 TUI 的 P03；本用例不把 RSS 单独当作正文泄漏证据。
#[tokio::test]
#[ignore = "P 矩阵测量（64/256/512 MiB 历史正文，真实 SQLite，资源敏感）"]
async fn p07_history_body_scale_curves() {
    let mib = env_usize("PERI_P07_MIB", 64);
    let body_kib = env_usize("PERI_P07_BODY_KIB", 64);
    let body_bytes = body_kib * 1024;
    let messages = (mib * 1024 * 1024) / body_bytes;
    let page_limit = env_usize("PERI_P07_PAGE_LIMIT", 50);
    let page_budget = env_usize("PERI_P07_PAGE_BUDGET", 512 * 1024);
    let pages_to_read = env_usize("PERI_P07_PAGES", 20);
    assert!(messages > 0, "PERI_P07_MIB 太小");

    let overall = Instant::now();
    let session = TestSession::open().await;
    let mut transcript =
        MessageTranscript::new().with_persistence(session.resources(), session.thread_id.clone());

    println!("== P07 环境 ==");
    print_memory("process(start)", &sample_process_memory());
    println!(
        "  目标={mib}MiB body/message={body_kib}KiB messages={messages} \
         page_limit={page_limit} page_budget={page_budget}B pages={pages_to_read}"
    );

    // 受控批次写入：每批 128 条（默认 8 MiB），低于生产默认待持久化预算
    // （1024 条 / 16 MiB），不为测量放宽准入规则。
    let batch = 128usize;
    let build_started = Instant::now();
    let mut archived_ids = Vec::with_capacity(messages);
    let mut appended = 0usize;
    while appended < messages {
        let count = batch.min(messages - appended);
        for index in appended..appended + count {
            let message = if index % 2 == 0 {
                make_human_body(&format!("P07-{index:06}-H"), body_bytes)
            } else {
                make_ai_body(&format!("P07-{index:06}-A"), body_bytes)
            };
            archived_ids.push(transcript.append(message));
        }
        transcript.flush_persistence().await.unwrap();
        appended += count;
    }
    let build_elapsed = build_started.elapsed();
    println!("  夹具生成：{:.3}s", build_elapsed.as_secs_f64());

    let mut stages: Vec<P07Stage> = Vec::new();
    let sample =
        |name: &'static str, transcript: &MessageTranscript, started: Instant| -> P07Stage {
            let snapshot = transcript.memory_snapshot();
            let memory = sample_process_memory();
            print_transcript(name, &snapshot);
            print_memory(name, &memory);
            P07Stage {
                name,
                snapshot,
                memory,
                elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
            }
        };

    // T0：compact 前。
    let stage_started = Instant::now();
    stages.push(sample("T0", &transcript, stage_started));
    let t0 = stages.last().unwrap().snapshot;
    assert_eq!(t0.own_entries, messages);
    assert!(
        t0.own_bytes.body >= mib * 1024 * 1024,
        "T0 逻辑正文必须覆盖目标规模"
    );

    // T1：真实 store 提交 + transcript 收缩。
    let flagged = own_flag_updates(&transcript, 0);
    let summary = make_summary(0);
    let commit_started = Instant::now();
    transcript
        .commit_compaction_lifecycle(CompactionChange {
            flag_updates: flagged,
            appended_messages: vec![summary],
        })
        .await
        .unwrap();
    let commit_elapsed = commit_started.elapsed();
    let stage_started = Instant::now();
    stages.push(sample("T1", &transcript, stage_started));
    let t1 = stages.last().unwrap().snapshot;
    assert_eq!(t1.own_entries, 1, "T1 只应保留摘要");
    assert_eq!(t1.own_bytes.body, summary_body_bytes());
    assert_ids_absent(&transcript, &archived_ids);

    // T2：交接快照只含有效集合。
    let stage_started = Instant::now();
    let handoff = transcript.active_payloads();
    let handoff_bytes: usize = handoff
        .iter()
        .map(|payload| crate::session::memory_probe::payload_bytes(payload).body)
        .sum();
    println!(
        "  T2 handoff_payloads={} handoff_body={}B",
        handoff.len(),
        handoff_bytes
    );
    stages.push(sample("T2", &transcript, stage_started));
    drop(handoff);

    // T3：排空持久化队列与临时引用。
    let stage_started = Instant::now();
    transcript.flush_persistence().await.unwrap();
    stages.push(sample("T3", &transcript, stage_started));

    // T4：长任务继续工作 8 条（固定 512 KiB）。
    let stage_started = Instant::now();
    let mut continued_body = 0usize;
    for index in 0..8 {
        let message = if index % 2 == 0 {
            make_human_body(&format!("P07-CONT-{index:02}-H"), body_bytes)
        } else {
            make_ai_body(&format!("P07-CONT-{index:02}-A"), body_bytes)
        };
        continued_body += message_bytes(&message).body;
        transcript.append(message);
    }
    transcript.flush_persistence().await.unwrap();
    stages.push(sample("T4", &transcript, stage_started));
    assert_eq!(
        stages.last().unwrap().snapshot.own_bytes.body,
        summary_body_bytes() + continued_body
    );

    // T5：任务结束。
    let stage_started = Instant::now();
    let final_message = make_ai_body("P07-FINAL-A", body_bytes);
    let final_body = message_bytes(&final_message).body;
    transcript.append(final_message);
    transcript.flush_persistence().await.unwrap();
    stages.push(sample("T5", &transcript, stage_started));
    let t5 = stages.last().unwrap().snapshot;
    assert_eq!(
        t5.own_bytes.body,
        summary_body_bytes() + continued_body + final_body
    );
    assert_ids_absent(&transcript, &archived_ids);

    // T6：存储级向更早翻页若干页，再重新读取有效尾部。
    let stage_started = Instant::now();
    let memory_before_pages = sample_process_memory();
    let mut cursor: Option<peri_acp_types::history_page::HistoryCursor> = None;
    let mut pages_read = 0usize;
    let mut items_read = 0usize;
    let mut total_page_bytes = 0usize;
    let mut max_page_bytes = 0usize;
    let mut oversize_items = 0usize;
    for _ in 0..pages_to_read {
        let page = session
            .resources()
            .load_history_page(&session.thread_id, cursor.as_ref(), page_limit, page_budget)
            .await
            .unwrap();
        let bytes = page.total_bytes();
        assert!(
            bytes <= page_budget || page.items.iter().any(|item| item.oversized),
            "普通页不得超过字节预算：{bytes} > {page_budget}"
        );
        oversize_items += page.items.iter().filter(|item| item.oversized).count();
        pages_read += 1;
        items_read += page.items.len();
        total_page_bytes += bytes;
        max_page_bytes = max_page_bytes.max(bytes);
        cursor = page.next_cursor.clone();
        drop(page);
        if cursor.is_none() {
            break;
        }
    }
    let active = session
        .resources()
        .load_active_snapshot(&session.thread_id)
        .await
        .unwrap();
    let active_payloads = crate::session::memory_probe::payload_set_snapshot(
        &active.payloads,
        active.payloads.capacity(),
    );
    stages.push(sample("T6", &transcript, stage_started));
    let t6 = stages.last().unwrap().snapshot;

    // 翻页后有效尾部身份/规模不变；归档正文没有借分页回到热态。
    assert_eq!(t6.own_entries, t5.own_entries);
    assert_eq!(t6.own_bytes.body, t5.own_bytes.body);
    assert_ids_absent(&transcript, &archived_ids);
    let expected_active = 1 + 8 + 1;
    assert_eq!(
        active.payloads.len(),
        expected_active,
        "有效尾部必须只有摘要 + 继续工作 + 最终消息"
    );
    assert!(
        active_payloads.bytes.body < mib * 1024 * 1024 / 4,
        "有效尾部不得接近归档规模"
    );
    println!(
        "  T6 paging pages={pages_read} items={items_read} total_page_bytes={total_page_bytes}B \
         max_page_bytes={max_page_bytes}B oversize={oversize_items} \
         active_payloads={} active_body={}B",
        active.payloads.len(),
        active_payloads.bytes.body
    );
    print_memory("T6 paging before", &memory_before_pages);
    print_memory("T6 paging after", &stages.last().unwrap().memory);

    println!("== P07 阶段汇总（{mib} MiB） ==");
    for stage in &stages {
        println!(
            "  {} entries={} body={}B reserved={}B entries_cap_bytes={} \
             id_index_cap_bytes={} flags_cap_bytes={} rss={} footprint={} malloc_in_use={} stage_ms={:.1}",
            stage.name,
            stage.snapshot.own_entries,
            stage.snapshot.own_bytes.body,
            stage.snapshot.own_bytes.reserved,
            stage.snapshot.entries_capacity_bytes,
            stage.snapshot.id_index_capacity_bytes,
            stage.snapshot.flags_capacity_bytes,
            fmt_mib(stage.memory.rss_bytes),
            fmt_mib(stage.memory.phys_footprint_bytes),
            fmt_mib(stage.memory.malloc_size_in_use),
            stage.elapsed_ms,
        );
    }
    println!(
        "P07 汇总：mib={mib} messages={messages} build_s={:.3} commit_ms={:.3} \
         pages={pages_read} page_total={}B page_max={}B active_body={}B total_s={:.3}",
        build_elapsed.as_secs_f64(),
        commit_elapsed.as_secs_f64() * 1000.0,
        total_page_bytes,
        max_page_bytes,
        active_payloads.bytes.body,
        overall.elapsed().as_secs_f64(),
    );

    transcript.shutdown_persistence();
}
