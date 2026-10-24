//! 展示区间摘除算法：槽位一致性、live 保留与失败不修改。

use super::remove_displayed_ranges;
use crate::kit::tui_render_unit::{TuiRenderUnit, TuiUserBubble};

fn units(count: usize) -> im::Vector<TuiRenderUnit> {
    (0..count)
        .map(|index| TuiRenderUnit::TuiUserBubble(TuiUserBubble::new(format!("u{index}"))))
        .collect()
}

fn texts(committed: &im::Vector<TuiRenderUnit>) -> Vec<String> {
    committed
        .iter()
        .map(|unit| match unit {
            TuiRenderUnit::TuiUserBubble(bubble) => bubble.text.clone(),
            other => panic!("unexpected unit: {other:?}"),
        })
        .collect()
}

#[test]
fn removes_displayed_ranges_in_place_and_keeps_live_units() {
    let mut committed = units(5);
    let (slot, removed_tools) =
        remove_displayed_ranges(&mut committed, &[(1, 2)]).expect("valid ranges");
    assert_eq!(slot, 1, "open 页插到首个被摘除区间的起点");
    assert!(removed_tools.is_empty());
    assert_eq!(texts(&committed), vec!["u0", "u3", "u4"]);
}

#[test]
fn empty_ranges_insert_at_tail_without_mutation() {
    let mut committed = units(3);
    let (slot, removed_tools) = remove_displayed_ranges(&mut committed, &[]).expect("empty ranges");
    assert_eq!(slot, 3);
    assert!(removed_tools.is_empty());
    assert_eq!(texts(&committed), vec!["u0", "u1", "u2"]);
}

#[test]
fn out_of_bounds_range_is_rejected_without_mutation() {
    let mut committed = units(3);
    let error = remove_displayed_ranges(&mut committed, &[(2, 2)]).expect_err("out of bounds");
    assert_eq!(error.ranges, vec![(2, 2)]);
    assert_eq!(error.committed_len, 3);
    assert_eq!(
        texts(&committed),
        vec!["u0", "u1", "u2"],
        "校验失败不得修改 committed"
    );
}

#[test]
fn overlapping_ranges_are_rejected_without_mutation() {
    let mut committed = units(5);
    remove_displayed_ranges(&mut committed, &[(0, 2), (1, 2)]).expect_err("overlap");
    assert_eq!(
        texts(&committed),
        vec!["u0", "u1", "u2", "u3", "u4"],
        "校验失败不得修改 committed"
    );
}

#[test]
fn zero_length_range_is_rejected() {
    let mut committed = units(2);
    remove_displayed_ranges(&mut committed, &[(1, 0)]).expect_err("zero length");
    assert_eq!(texts(&committed), vec!["u0", "u1"]);
}
