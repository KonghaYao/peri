use ratatui_kit::prelude::*;

use super::{AutoFollowCtx, SCROLL_PADDING};

// ── 粘性吸底跟随状态 ───────────────────────────────────────────────────

/// 用户滚动后是否应恢复吸底跟随：只有滚到真正底部（offset ≥ max_scroll，
/// 含 Down 溢出 / End / usize::MAX 哨兵）才恢复。
/// [Why 严格到底] 旧版用 proximity 阈值（视口 1/4）判定：loading 中用户上滚
/// ≤ 阈值会在下一次内容增长时被吸回，反复拉锯；且跟随态下内容跳增超过阈值时
/// 跟随被拒绝，视口停在半空、spinner 消失——体验"底部跳动"。
/// 粘性语义：一向上滚动即退出跟随（浏览模式），滚回底部才恢复。
/// [Fix padding] max_scroll 含 mod.rs 的 +2 滚动缓冲（SCROLL_PADDING）：若按它
/// 判定，用户滚到视觉底部（真实内容底 = max_scroll - 2）时 offset 恒差 2 行，
/// 吸底跟随永不恢复。扣除缓冲后「滚到视觉底部」即恢复；内容不满一屏
/// （max_scroll ≤ padding）时 offset=0 仍视为底部。
pub(super) fn should_follow_after_user_scroll(max_scroll: usize, offset_y: usize) -> bool {
    offset_y >= max_scroll.saturating_sub(SCROLL_PADDING)
}

/// 用户滚动入口（键盘 / 滚轮 / 滚动条）滚动落定后同步 follow_bottom。
/// write_no_update：事件 dispatch 后 loop 强制 render，无需 wake。
pub(super) fn update_follow_on_scroll(
    follow_bottom: &State<bool>,
    max_scroll: usize,
    offset_y: usize,
) {
    *follow_bottom.write_no_update() = should_follow_after_user_scroll(max_scroll, offset_y);
}

/// §8.1 `↓ New output` 指示器判定：浏览态（用户滚离底部，follow=false）且
/// 视口未到**真实内容底**时显示。
///
/// [Why 内容底口径] `content_bottom` = core + footer 视觉行数（**不含**
/// SCROLL_PADDING 缓冲——缓冲行不可见，滚到视觉底部即消失）。与粘性 follow
/// 恢复（`should_follow_after_user_scroll` 同样扣缓冲）口径对齐：滚到底时
/// follow 恢复 true 且指示器消失；浏览态中内容增长不移动 viewport，指示器
/// 出现提示有未看的新输出。
pub(in crate::kit::message_area) fn new_output_indicator_active(
    follow_bottom: bool,
    scroll_y: usize,
    vis_height: usize,
    content_bottom: usize,
) -> bool {
    !follow_bottom && scroll_y + vis_height < content_bottom
}

/// [Slice 4 §6.8] Interaction block 锚定对齐目标：pending block 末行超出视口
/// 时返回对齐偏移（block 底部对齐视口底部），否则 None（不调整）。
///
/// 纯函数——`run_auto_follow` 的 anchor 分支消费；浏览态与跟随态均生效
/// （§6.8「等待时锚定此 block」，不得被新 streaming chunk 滚出视口）。
pub(in crate::kit::message_area) fn anchor_scroll_target(
    scroll_y: usize,
    vis_height: usize,
    anchor_end: usize,
    max_scroll: usize,
) -> Option<usize> {
    if scroll_y.saturating_add(vis_height) < anchor_end {
        Some(anchor_end.saturating_sub(vis_height).min(max_scroll))
    } else {
        None
    }
}

// ── 吸底自动跟随 ─────────────────────────────────────────────────────────

/// 从 `use_effect` 闭包提取的吸底逻辑。
/// 注意：use_effect body 不是 render body，所以 `write()` 是正确的（需要 wake 触发后续渲染）。
pub(in crate::kit::message_area) fn run_auto_follow(ctx: &AutoFollowCtx) {
    // [Diagnostic] 记录每次 effect 触发的关键参数——trace 历史/submit 两个滚动问题。
    // [Perf] run_auto_follow 随 vm_generation 每 token 触发，info 级日志在默认
    // filter 下逐 token 同步落盘（RollingFileAppender），多 Agent 并发时放大为每秒
    // 数百次文件写。热路径诊断统一降为 trace 级，按需开启
    // `RUST_LOG=...msg_scroll_diag=trace` 排查；低频事件（submit/thread_load 等
    // consumer）仍保持 info。
    tracing::trace!(
        target: "msg_scroll_diag",
        items_len = ctx.items_len,
        total_rows = ctx.total_visual_rows,
        vis_h = ctx.vis_height,
        is_loading = ctx.is_loading,
        follow = *ctx.follow_bottom.read(),
        scroll_y = ctx.scroll_state.read().offset(),
        prev_lsa = *ctx.last_scrolled_at.read(),
        prev_items = *ctx.prev_items_len.read(),
        "auto_follow: entry",
    );

    // [Fix] resize 后 total_visual_rows 变化时，主动钳制 scroll_state.offset 到有效范围。
    let prev_total = *ctx.prev_total_visual_rows.read();
    *ctx.prev_total_visual_rows.write() = ctx.total_visual_rows;
    if prev_total != ctx.total_visual_rows && ctx.total_visual_rows > 0 && ctx.vis_height > 0 {
        let max_scroll = ctx
            .total_visual_rows
            .saturating_sub(ctx.vis_height as usize);
        let current_y = ctx.scroll_state.read().offset();
        if current_y > max_scroll {
            ctx.scroll_state.write().set_offset(max_scroll);
        }
    }

    // [Fix] resize 高度变化（vis_height 变）后跟随底部。
    // [Why] use_effect 依赖（items_len, vm_generation, is_loading, total_visual_rows）不含
    // vis_height，终端高度变化时 effect 不触发；而渲染侧 clamp 只在 offset > max_scroll
    // 时钳制上限——resize 缩小视口后 max_scroll 变大，offset 停在旧底部不再到底，底部的
    // footer（2 空行 + spinner）被挤出视口。
    // 判定改为 follow_bottom：跟随态（用户没在浏览）resize 后跟随到底；浏览态不打扰。
    // 旧版用 proximity 阈值（视口 1/4）判定，浏览态距底 ≤ 阈值时仍会被误拉。
    let prev_vis = *ctx.prev_vis_height.read();
    *ctx.prev_vis_height.write() = ctx.vis_height;
    if prev_vis != ctx.vis_height
        && *ctx.follow_bottom.read()
        && ctx.total_visual_rows > 0
        && ctx.vis_height > 0
    {
        tracing::trace!(
            target: "msg_scroll_diag",
            prev_vis,
            new_vis = ctx.vis_height,
            "auto_follow: resize (vis_height changed) → follow bottom",
        );
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
    }

    // ── [Fix #1] Submit 强制滚底：用户主动发送 prompt 时 LOADING_EPOCH 递增 ──
    // 当前 effect 可能在 user bubble 到达 VIEW_MODELS 之前触发（submit_consumer
    // 先设 is_loading=true，再 call prompt() RPC）。此时 scroll_to_bottom 定位
    // 到当前的底部位置即可——user bubble 到达后 proximity 自然跟随。
    let prev_epoch = *ctx.prev_loading_epoch.read();
    *ctx.prev_loading_epoch.write() = ctx.loading_epoch;
    if ctx.loading_epoch != prev_epoch && ctx.total_visual_rows > 0 && ctx.vis_height > 0 {
        tracing::trace!(
            target: "msg_scroll_diag",
            prev_epoch,
            new_epoch = ctx.loading_epoch,
            "auto_follow: submit detected (LOADING_EPOCH changed) → force scroll_to_bottom",
        );
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
        *ctx.follow_bottom.write() = true;
        // 不 return——继续走后续逻辑处理 user bubble / 流式增长
    }

    // ── [Fix #2] History 切换 / /clear 检测：BRIDGE_RESET_COUNTER 递增时重置哨兵 ──
    // prev_items_len←0 和 last_scrolled_at←0 一起作为「新会话首次批量加载」的哨兵：
    // 后续的 prev==0 分支（在所有 proximity guard 之前）强制每批 scroll_to_bottom，
    // 且不消费 prev==0（保持 trigger 活跃至 replay 结束）。
    let prev_ctr = *ctx.prev_reset_counter.read();
    *ctx.prev_reset_counter.write() = ctx.bridge_reset_counter;
    if ctx.bridge_reset_counter != prev_ctr {
        tracing::trace!(
            target: "msg_scroll_diag",
            prev_ctr,
            new_ctr = ctx.bridge_reset_counter,
            "auto_follow: BRIDGE_RESET_COUNTER changed → arming prev==0 force-scroll",
        );
        *ctx.prev_items_len.write() = 0;
        *ctx.last_scrolled_at.write() = 0;
    }

    // [TRAP] parking_lot 同 thread 死锁规避：先 read copy 出 owned，guard 在语句末尾 drop，再 write。
    let prev = *ctx.prev_items_len.read();

    // ── 零内容保护 ──
    if ctx.total_visual_rows == 0 || ctx.vis_height == 0 {
        *ctx.prev_items_len.write() = ctx.items_len;
        tracing::trace!(target: "msg_scroll_diag", "auto_follow: early return (zero total or vis)");
        return;
    }

    // ── [Fix #3] History replay 批量强制滚底（哨兵 prev==0）──
    // 仅在 non-loading 且 「BRIDGE_RESET_COUNTER 递增触发了 prev_items_len 归零」时进入。
    // 每批次都 force scroll + 再次将 prev_items_len 归零——直到 replay 结束，
    // generation 不再增长、effect 停发，prev==0 自然消弭。
    if prev == 0 && !ctx.is_loading && ctx.items_len > 0 {
        tracing::trace!(
            target: "msg_scroll_diag",
            items_len = ctx.items_len,
            "auto_follow: prev==0 force-scroll (history replay batch) → scroll_to_bottom",
        );
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
        *ctx.follow_bottom.write() = true;
        // 不消费 prev==0——维持为 0 让后续 batch 也走此路径
        *ctx.prev_items_len.write() = 0;
        return;
    }

    // ── 正常路径：更新 prev_items_len ──
    *ctx.prev_items_len.write() = ctx.items_len;

    // ── [Slice 4 §6.8] Interaction block 锚定 ──
    // pending interaction block 存在时，block 末行超出视口 → 视口对齐到 block
    // 底部。**仅跟随态生效**（§6.8 字面「等待时 follow mode 锚定此 block」）：
    // 浏览态（用户滚离底部）下新内容不得移动 viewport（§8.1），锚定分支在
    // `!follow_bottom` 早退之前——跟随态下锚定优先于粘性跟随判定；resize 后
    // 按新快照重算（prev_vis_height 路径共存，anchor 分支在其后覆盖对齐目标）。
    // block 完成（pending=false → 派生扫描不到 → None）后恢复原语义，不强制
    // follow（§15）。[Fix] 浏览态下每帧被拉回 block 底部会打断用户阅读——
    // 与 §8.1「浏览态新内容不得移动 viewport」相悖，改为浏览态跳过锚定。
    if *ctx.follow_bottom.read()
        && let Some((_anchor_start, anchor_end)) = ctx.anchor_visual_range
    {
        let max_scroll = ctx
            .total_visual_rows
            .saturating_sub(ctx.vis_height as usize);
        let scroll_y = ctx.scroll_state.read().offset();
        if let Some(target) =
            anchor_scroll_target(scroll_y, ctx.vis_height as usize, anchor_end, max_scroll)
        {
            tracing::trace!(
                target: "msg_scroll_diag",
                anchor_end,
                scroll_y,
                target,
                "auto_follow: interaction anchor → align viewport to block bottom",
            );
            ctx.scroll_state.write().set_offset(target);
            *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
        }
        return;
    }

    // ── 粘性跟随 guard ──
    // [Why] 用户一旦向上滚动（offset < max_scroll，update_follow_on_scroll 已置
    // false）即进入浏览模式：内容增长不再吸回，可自由翻看历史。
    // 只有滚回真正底部（或 submit / replay / shrink 等结构性事件）才恢复跟随。
    // 旧版只有 proximity 阈值（视口 1/4）：loading 中上滚 ≤ 阈值会在下一次内容
    // 增长时被吸回，反复拉锯；且内容单帧跳增超过阈值时跟随被拒绝，视口停在
    // 半空、spinner 消失——底部跳动。粘性语义下这两类问题都不存在。
    if !*ctx.follow_bottom.read() {
        tracing::trace!(target: "msg_scroll_diag", "auto_follow: browsing (follow=false) → skip");
        return;
    }

    let prev_lsa = *ctx.last_scrolled_at.read();

    if ctx.is_loading {
        if ctx.total_visual_rows > prev_lsa {
            tracing::trace!(target: "msg_scroll_diag", "auto_follow: loading → scroll_to_bottom");
            ctx.scroll_state.write().scroll_to_bottom();
            *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
        } else {
            tracing::trace!(target: "msg_scroll_diag", total = ctx.total_visual_rows, prev_lsa, "auto_follow: loading → skip (total_rows not greater than prev_lsa)");
        }
        return;
    }

    if ctx.items_len < prev {
        tracing::trace!(target: "msg_scroll_diag", items_len = ctx.items_len, prev, "auto_follow: shrink → scroll_to_bottom");
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
        *ctx.follow_bottom.write() = true;
        return;
    }

    if ctx.total_visual_rows > prev_lsa {
        tracing::trace!(target: "msg_scroll_diag", "auto_follow: non-loading growth → scroll_to_bottom");
        ctx.scroll_state.write().scroll_to_bottom();
        *ctx.last_scrolled_at.write() = ctx.total_visual_rows;
    } else {
        tracing::trace!(target: "msg_scroll_diag", total = ctx.total_visual_rows, prev_lsa, "auto_follow: non-loading → skip (total_rows not greater than prev_lsa)");
    }
}
