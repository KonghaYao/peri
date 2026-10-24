//! live 展示单元到持久条目的关联（仅 TUI 侧）。
//!
//! 窗口模型只接受 `PersistentEntryKey` 才能建立可淘汰段。本模块保存三类可核实
//! 的关联：
//!
//! 1. 工具事件携带真实所属 assistant `originMessageId` 时（含纯工具响应轮次），
//!    工具卡直接归属该消息。
//! 2. 旧服务端没有 `originMessageId` 时，回退到流式 assistant 文本/推理块建立的
//!    **当前消息身份**（同一 `TurnSegment` 顺序内 tool 紧跟该消息的文本/推理）。
//! 3. 窗口 open 页的工具子项与 `committed` 中 live 卡片按 `tool_id` 精确匹配时，
//!    页条目的持久身份就是该卡片的归属（`RetainedLiveEntries` 写入）。
//!
//! 三者都只是**展示归属**，不改变 wire 字段语义。没有可核实关联的工具卡继续按
//! 未归属正文计入预算，不伪造持久身份。
//!
//! 状态按宿主 `active_session_id` 分区（与 `history_page::PendingToolResults`
//! 同口径）；桥接串行消费，故用进程级互斥量。分区不能全局清空：并行测试/多会话
//! 会互相擦除。旧会话分区按最近使用淘汰，避免长进程累积。

use std::collections::{HashMap, VecDeque};

use parking_lot::Mutex;

use crate::kit::history_window::PersistentEntryKey;

/// 工具卡所属的持久条目及其（可能未知的）存储序位置。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolOwner {
    pub entry: PersistentEntryKey,
    pub position: Option<u64>,
}

struct SessionLinks {
    /// 最近一次访问序号，用于分区淘汰。
    last_used: u64,
    /// 当前 open 的 assistant 消息身份；新 chunk 覆盖，turn 边界清空。
    current_message_id: Option<String>,
    /// `tool_id -> assistant message id`（live 归属）。
    tool_messages: HashMap<String, String>,
    /// 两份工具映射的插入顺序（每个 `tool_id` 至多一项；重复登记移到末尾）。
    tool_order: VecDeque<String>,
    /// `tool_id -> 页条目`（open adopt 提供的持久定位）。
    page_entries: HashMap<String, ToolOwner>,
    /// 已按未归属正文计入预算的工具卡字节（用于归属后回落）。
    ///
    /// 键含窗口实例与代次：预算重置后旧记录不得压制新窗口的首次记账。
    unattributed_bytes: HashMap<(u64, u64, String), usize>,
}

impl SessionLinks {
    fn new(last_used: u64) -> Self {
        Self {
            last_used,
            current_message_id: None,
            tool_messages: HashMap::new(),
            tool_order: VecDeque::new(),
            page_entries: HashMap::new(),
            unattributed_bytes: HashMap::new(),
        }
    }

    /// 记录一次工具关联的新鲜度（LRU 语义：重复登记移到队尾）。
    fn touch_tool(&mut self, tool_id: &str) {
        if let Some(index) = self.tool_order.iter().position(|id| id == tool_id) {
            self.tool_order.remove(index);
        }
        self.tool_order.push_back(tool_id.to_string());
    }

    /// 两份映射的并集超过单会话上限时，淘汰最旧的工具关联。
    ///
    /// 已归属段（LiveSeal/页段）的预算与释放只依赖段和展示账目，不再查询这
    /// 两份映射，因此淘汰旧关联不会改变已归属段的释放语义；代价是超过上限的
    /// 陈旧工具若再次归档，可能失去回填证据而重新计入未归属。
    fn enforce_tool_bound(&mut self) {
        while self.tool_order.len() > MAX_TOOL_LINKS_PER_SESSION {
            let Some(oldest) = self.tool_order.pop_front() else {
                break;
            };
            self.tool_messages.remove(&oldest);
            self.page_entries.remove(&oldest);
        }
    }
}

/// 同时保留的会话分区上限。超过后淘汰最久未使用的**其他**会话。
const MAX_SESSIONS: usize = 8;

/// 单会话保留的工具关联上限（`tool_messages` 与 `page_entries` 的并集）。
///
/// 工具卡归档后会转为可淘汰段，关联只在尚未归属/回填时有用；固定上限让长
/// 会话的映射内存有界。
const MAX_TOOL_LINKS_PER_SESSION: usize = 1024;

struct Links {
    clock: u64,
    sessions: HashMap<String, SessionLinks>,
}

static LINKS: Mutex<Option<Links>> = Mutex::new(None);

fn with_links<R>(session_id: &str, f: impl FnOnce(&mut SessionLinks) -> R) -> R {
    let mut guard = LINKS.lock();
    let links = guard.get_or_insert_with(|| Links {
        clock: 0,
        sessions: HashMap::new(),
    });
    links.clock = links.clock.saturating_add(1);
    let clock = links.clock;
    let session = links
        .sessions
        .entry(session_id.to_string())
        .or_insert_with(|| SessionLinks::new(clock));
    session.last_used = clock;
    let result = f(session);
    if links.sessions.len() > MAX_SESSIONS {
        let oldest = links
            .sessions
            .iter()
            .filter(|(id, _)| id.as_str() != session_id)
            .min_by_key(|(_, links)| links.last_used)
            .map(|(id, _)| id.clone());
        if let Some(oldest) = oldest {
            links.sessions.remove(&oldest);
        }
    }
    result
}

/// 记录根 agent 的流式 chunk 身份。`None` 表示该 chunk 无法提供身份，后续工具卡
/// 不得沿用更早消息的 id（保守放弃归属）。
pub(crate) fn note_assistant_chunk(session_id: &str, message_id: Option<&str>) {
    with_links(session_id, |links| {
        links.current_message_id = message_id.map(str::to_owned);
    });
}

/// 根 agent 工具开始：优先使用事件携带的真实所属 assistant 消息身份
/// （`originMessageId`，纯工具轮次也能归属）；缺失时才回退到流式 chunk 记录的
/// 当前消息身份。两者都没有则保持未归属，不用 turn 身份冒充。
pub(crate) fn note_root_tool_started(
    session_id: &str,
    tool_id: &str,
    origin_message_id: Option<&str>,
) {
    with_links(session_id, |links| {
        let message_id = origin_message_id
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .or_else(|| links.current_message_id.clone());
        if let Some(message_id) = message_id {
            links.tool_messages.insert(tool_id.to_string(), message_id);
            links.touch_tool(tool_id);
            links.enforce_tool_bound();
        }
    });
}

/// 根 agent 工具结束：该 assistant 消息的下一段输出可能没有文本，不能把旧 id
/// 沿用到下一个响应。
///
/// 只有结束的工具确实属于当前流式消息时才清除身份：迟到的上一轮 `ToolEnded`
/// 不得擦掉新一轮已建立的消息归属（跨 turn 串扰）。
pub(crate) fn note_root_tool_ended(session_id: &str, tool_id: &str) {
    with_links(session_id, |links| {
        let ended_owner = links.tool_messages.get(tool_id);
        let same_message = match (&links.current_message_id, ended_owner) {
            (Some(current), Some(owner)) => current == owner,
            // 该工具开始时就未能建立归属（或当前没有身份）：它无法证明当前
            // 身份属于自己，迟到的终点不得擦除之后由 chunk 建立的较新身份。
            _ => false,
        };
        if same_message {
            links.current_message_id = None;
        }
    });
}

/// turn 边界（done / interrupt / suspend / replay）：消息身份不跨轮沿用。
///
/// `tool_id -> message` 归属本身保留到卡片释放：归档后的工具卡仍靠它在窗口里
/// 建可淘汰段；tool_id 唯一，不会把归属串到下一轮的其他工具。
pub(crate) fn note_turn_boundary(session_id: &str) {
    with_links(session_id, |links| {
        links.current_message_id = None;
    });
}

/// 工具卡的持久归属：优先 live 消息身份，其次 open 页给出的条目。
pub(crate) fn owner_for_tool(session_id: &str, tool_id: &str) -> Option<ToolOwner> {
    with_links(session_id, |links| {
        if let Some(message_id) = links.tool_messages.get(tool_id) {
            return Some(ToolOwner {
                entry: PersistentEntryKey::own(format!("{session_id}:{message_id}")),
                position: None,
            });
        }
        links.page_entries.get(tool_id).cloned()
    })
}

/// 绑定 open 页提供的工具持久定位；返回本次**新增或改变**的 tool_id。
pub(crate) fn bind_page_entries(
    session_id: &str,
    entries: &[(String, PersistentEntryKey, Option<u64>)],
) -> Vec<String> {
    with_links(session_id, |links| {
        let mut changed = Vec::new();
        for (tool_id, entry, position) in entries {
            let owner = ToolOwner {
                entry: entry.clone(),
                position: *position,
            };
            if links.page_entries.get(tool_id) != Some(&owner) {
                links.page_entries.insert(tool_id.clone(), owner);
                links.touch_tool(tool_id);
                changed.push(tool_id.clone());
            }
        }
        links.enforce_tool_bound();
        changed
    })
}

/// 记录按未归属正文计入预算的工具卡字节；返回相对**同一窗口代次**上次记录的
/// 增量（可能为负）。预算重置后旧代次记录被清理，首次记账返回完整字节。
pub(crate) fn note_unattributed(
    session_id: &str,
    window_id: u64,
    generation: u64,
    tool_id: &str,
    bytes: usize,
) -> i64 {
    with_links(session_id, |links| {
        let key = (window_id, generation, tool_id.to_string());
        let previous = links.unattributed_bytes.get(&key).copied().unwrap_or(0);
        // 同一工具卡在旧窗口代次下的记录已不属于当前预算。
        links
            .unattributed_bytes
            .retain(|(window, record_generation, id), _| {
                !(id == tool_id && (*window != window_id || *record_generation != generation))
            });
        links.unattributed_bytes.insert(key, bytes);
        bytes as i64 - previous as i64
    })
}

/// 取出并移除当前窗口代次的未归属记录；归属成功时调用，使
/// `unattributed_live_bytes` 回落。
pub(crate) fn take_unattributed(
    session_id: &str,
    window_id: u64,
    generation: u64,
    tool_id: &str,
) -> usize {
    with_links(session_id, |links| {
        let taken = links
            .unattributed_bytes
            .remove(&(window_id, generation, tool_id.to_string()))
            .unwrap_or_default();
        links
            .unattributed_bytes
            .retain(|(window, record_generation, id), _| {
                !(id == tool_id && (*window != window_id || *record_generation != generation))
            });
        taken
    })
}

/// 迟到工具更新改变未归属卡片字节时同步账目；返回需要应用到窗口的差值。
/// 未登记为当前窗口代次的未归属正文（例如页卡片）时返回 0。
pub(crate) fn resize_unattributed(
    session_id: &str,
    window_id: u64,
    generation: u64,
    tool_id: &str,
    new_bytes: usize,
) -> i64 {
    with_links(session_id, |links| {
        let key = (window_id, generation, tool_id.to_string());
        let Some(previous) = links.unattributed_bytes.get(&key).copied() else {
            return 0;
        };
        links.unattributed_bytes.insert(key, new_bytes);
        new_bytes as i64 - previous as i64
    })
}

/// 展示单元被释放后清掉关联，避免跨窗口代次的悬挂引用。
pub(crate) fn forget_tool(session_id: &str, tool_id: &str) {
    with_links(session_id, |links| {
        links.tool_messages.remove(tool_id);
        links.page_entries.remove(tool_id);
        links.tool_order.retain(|id| id != tool_id);
        links
            .unattributed_bytes
            .retain(|(_, _, id), _| id != tool_id);
    });
}

/// 会话边界（rewind / reset / 会话切换）丢弃整份关联。
///
/// 这些路径会清空或替换 `committed`：保留 `tool_id -> message` 映射只会在长进程里
/// 无界累积，且对应卡片已没有展示对象。分区被整体移除，下次访问重新建立。
pub(crate) fn forget_session(session_id: &str) {
    if let Some(links) = LINKS.lock().as_mut() {
        links.sessions.remove(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner_entry(session_id: &str, tool_id: &str) -> Option<String> {
        owner_for_tool(session_id, tool_id).map(|owner| owner.entry.entry_id)
    }

    /// 显式 `originMessageId` 优先于流式启发式；这是纯工具轮次的唯一身份来源。
    #[test]
    fn explicit_origin_wins_over_streaming_identity() {
        let session = "links-origin-priority";
        note_assistant_chunk(session, Some("m-stream"));
        note_root_tool_started(session, "tool", Some("m-origin"));
        assert_eq!(
            owner_entry(session, "tool").as_deref(),
            Some("links-origin-priority:m-origin")
        );
        forget_tool(session, "tool");
    }

    /// 旧服务端没有 `originMessageId` 时才回退到流式记录的身份。
    #[test]
    fn missing_origin_falls_back_to_streaming_identity() {
        let session = "links-fallback";
        note_assistant_chunk(session, Some("m-stream"));
        note_root_tool_started(session, "tool", None);
        assert_eq!(
            owner_entry(session, "tool").as_deref(),
            Some("links-fallback:m-stream")
        );

        // 空串不是可靠身份，同样回退。
        note_root_tool_started(session, "tool-empty", Some(""));
        assert_eq!(
            owner_entry(session, "tool-empty").as_deref(),
            Some("links-fallback:m-stream")
        );
        forget_tool(session, "tool");
        forget_tool(session, "tool-empty");
    }

    /// 完全没有身份时不伪造归属。
    #[test]
    fn no_identity_stays_unattributed() {
        let session = "links-unattributed";
        note_turn_boundary(session);
        note_root_tool_started(session, "tool", None);
        assert!(owner_entry(session, "tool").is_none());
    }

    /// 迟到 ToolEnded 不得擦掉下一轮已由 chunk 建立的身份，也不得把旧工具
    /// 改挂到新消息。
    #[test]
    fn late_tool_end_does_not_clear_newer_identity() {
        let session = "links-late-end";
        note_assistant_chunk(session, Some("m-first"));
        note_root_tool_started(session, "tool-first", None);
        // 下一轮（例如纯工具轮次）先记录新消息身份。
        note_assistant_chunk(session, Some("m-second"));
        note_root_tool_ended(session, "tool-first");
        note_root_tool_started(session, "tool-second", None);
        assert_eq!(
            owner_entry(session, "tool-second").as_deref(),
            Some("links-late-end:m-second"),
            "迟到终点不得清掉新一轮身份"
        );
        assert_eq!(
            owner_entry(session, "tool-first").as_deref(),
            Some("links-late-end:m-first"),
            "旧工具归属保持不变"
        );

        // 本轮工具结束后，后续没有身份的卡片不再沿用旧消息。
        note_root_tool_ended(session, "tool-second");
        note_root_tool_started(session, "tool-third", None);
        assert!(owner_entry(session, "tool-third").is_none());
        forget_tool(session, "tool-first");
        forget_tool(session, "tool-second");
    }

    /// turn 边界清掉启发式身份，但保留按 tool_id 的归属，供归档/释放使用。
    #[test]
    fn turn_boundary_clears_heuristic_but_keeps_tool_links() {
        let session = "links-turn-boundary";
        note_assistant_chunk(session, Some("m-live"));
        note_root_tool_started(session, "tool", None);
        note_turn_boundary(session);
        note_root_tool_started(session, "tool-next", None);
        assert!(owner_entry(session, "tool-next").is_none());
        assert_eq!(
            owner_entry(session, "tool").as_deref(),
            Some("links-turn-boundary:m-live")
        );
        forget_tool(session, "tool");
    }

    /// 释放后清掉 live 与页关联，避免跨窗口代次悬挂。
    #[test]
    fn forget_tool_drops_all_links() {
        let session = "links-forget";
        note_root_tool_started(session, "tool", Some("m-origin"));
        forget_tool(session, "tool");
        assert!(owner_entry(session, "tool").is_none());
    }

    /// 单会话映射有界：超过上限后淘汰最旧关联，最新关联仍可解析。
    #[test]
    fn tool_links_per_session_are_bounded() {
        let session = "links-bounded";
        forget_session(session);
        let total = MAX_TOOL_LINKS_PER_SESSION + 32;
        for index in 0..total {
            let tool_id = format!("tool-{index}");
            note_root_tool_started(session, &tool_id, Some("m"));
            bind_page_entries(
                session,
                &[(
                    tool_id,
                    PersistentEntryKey::own(format!("s1:m-{index}")),
                    Some(index as u64),
                )],
            );
        }

        let guard = LINKS.lock();
        let links = guard
            .as_ref()
            .and_then(|links| links.sessions.get(session))
            .expect("session partition");
        assert_eq!(links.tool_messages.len(), MAX_TOOL_LINKS_PER_SESSION);
        assert_eq!(links.page_entries.len(), MAX_TOOL_LINKS_PER_SESSION);
        assert_eq!(links.tool_order.len(), MAX_TOOL_LINKS_PER_SESSION);
        assert!(
            !links.tool_messages.contains_key("tool-0"),
            "最旧的 tool_messages 关联被淘汰"
        );
        assert!(
            !links.page_entries.contains_key("tool-0"),
            "最旧的 page_entries 关联被淘汰"
        );
        let newest = format!("tool-{}", total - 1);
        assert!(links.tool_messages.contains_key(&newest));
        assert!(links.page_entries.contains_key(&newest));
        drop(guard);
        forget_session(session);
    }
}
