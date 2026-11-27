use super::{
    BodyGapGroupCache, FoldKey, FoldState, GROUP_CUT_CAP, GroupCut, GroupCutAnchor, TuiRenderUnit,
    build_collapsed_group, is_body_unit,
};
use crate::kit::tui_render_unit::history_overlap;

/// 重建 `[input_base..]` 后缀的分组输出（追加到 `grouped`，切点追加到 `cuts`）。
pub(super) fn rebuild_grouped_suffix(
    suffix: im::Vector<TuiRenderUnit>,
    input_base: usize,
    grouped: &mut im::Vector<TuiRenderUnit>,
    cuts: &mut Vec<GroupCut>,
    overrides: &std::collections::HashMap<FoldKey, FoldState>,
    detail_hashes: &std::collections::HashSet<u64>,
) {
    // 单游标推进：正文段与过程段共享同一个迭代器，避免对每段重复 skip 开头。
    let mut iter = suffix.iter().enumerate().peekable();
    while let Some((offset, unit)) = iter.next() {
        if is_body_unit(unit, detail_hashes) {
            grouped.push_back(unit.clone());
            // 正文逐条透传：每个边界都是稳定切点，尾部变化时可就近复用前缀。
            cuts.push(GroupCut {
                input: input_base + offset + 1,
                output: grouped.len(),
                anchor: GroupCutAnchor::AfterBody,
            });
            continue;
        }
        let mut run = vec![unit.clone()];
        let mut consumed = offset + 1;
        while let Some((next_offset, next)) = iter.peek() {
            if is_body_unit(next, detail_hashes) {
                break;
            }
            run.push((**next).clone());
            consumed = next_offset + 1;
            iter.next();
        }
        cuts.push(GroupCut {
            input: input_base + offset,
            output: grouped.len(),
            anchor: GroupCutAnchor::AfterBody,
        });
        grouped.push_back(build_collapsed_group(run, overrides));
        cuts.push(GroupCut {
            input: input_base + consumed,
            output: grouped.len(),
            anchor: GroupCutAnchor::AfterGroup,
        });
    }
}

/// A body closes a process group on either side. Regroup only through the seam; the
/// remainder, including the live tail's Working marker, still has the same inputs.
pub(super) fn group_prepended_page(
    cached: &BodyGapGroupCache,
    input: &im::Vector<TuiRenderUnit>,
    overrides: &std::collections::HashMap<FoldKey, FoldState>,
    detail_hashes: &std::collections::HashSet<u64>,
) -> Option<(im::Vector<TuiRenderUnit>, Vec<GroupCut>)> {
    let run = history_overlap(&cached.input, input);
    if run.len == 0
        || run.old_start + run.len != cached.input.len()
        || run.new_start + run.len != input.len()
    {
        return None;
    }
    let mut old_output = 0;
    let mut in_group = false;
    let mut old_body = 0;
    for (slot, unit) in cached.input.iter().enumerate() {
        let body = is_body_unit(unit, detail_hashes);
        if slot >= run.old_start && body {
            old_body = slot;
            break;
        }
        old_output += usize::from(body || !in_group);
        in_group = !body;
        old_body = slot + 1;
    }
    if old_body == cached.input.len() {
        return None;
    }
    let new_body = run.new_start + (old_body - run.old_start);
    let mut before = input.clone();
    before.split_off(new_body);
    let mut grouped = im::Vector::new();
    let mut cuts = Vec::new();
    rebuild_grouped_suffix(before, 0, &mut grouped, &mut cuts, overrides, detail_hashes);
    let new_output = grouped.len();
    grouped = crate::kit::tui_render_unit::prepend_units(
        grouped,
        cached.grouped.clone().split_off(old_output),
    );
    cuts.extend(
        cached
            .cuts
            .iter()
            .filter(|cut| cut.input > old_body)
            .map(|cut| GroupCut {
                input: new_body + (cut.input - old_body),
                output: new_output + (cut.output - old_output),
                anchor: cut.anchor,
            }),
    );
    if cuts.len() > GROUP_CUT_CAP {
        cuts.drain(..cuts.len() - GROUP_CUT_CAP);
    }
    Some((grouped, cuts))
}

#[test]
#[serial_test::serial]
fn history_page_rebuilt_process_group_before_todo_keeps_live_working_state() {
    use crate::kit::tui_render_unit::{TuiNoteLevel, TuiSystemNote, TuiTodoSummary};
    crate::kit::atoms::init_atoms();
    super::reset_body_gap_group_cache();
    let note = |n| {
        TuiRenderUnit::TuiSystemNote(TuiSystemNote {
            text: format!("note {n}"),
            level: TuiNoteLevel::Info,
            content_hash: n,
        })
    };
    let raw = im::vector![
        note(1),
        TuiRenderUnit::TuiTodoSummary(TuiTodoSummary::new("tasks".into()))
    ];
    let mut first = raw.clone();
    super::group_body_gaps(&mut first, &Default::default(), &Default::default(), true);
    let mut page = raw;
    page.push_front(note(2));
    super::group_body_gaps(&mut page, &Default::default(), &Default::default(), true);
    assert!(
        matches!(&page[0], TuiRenderUnit::TuiCollapsedGroup(group) if group.is_working && group.count == 2)
    );
    super::reset_body_gap_group_cache();
}
