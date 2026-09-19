//! [EPHEMERAL] 快照组装成本对照——`im::Vector::append` vs `push_back` 循环。
//!
//! 动机：`push_view_models` 的 assemble 阶段实测（release，N=1000）
//! 空 current_turn = 0.1 µs，1 个元素的 current_turn = 129.5 µs。差异只来自
//! `items.append(current_turn.view_models().clone())`——im 的 `append` 在
//! 「大向量接小向量」路径上退化为整树重建（成本 ∝ `self.len()`），而
//! `push_back` 只复制右脊与右端 chunk。`split_off` 同样有按位置的固定开销。
//! 两者的实测曲线是 `render.rs` 中 `join_into` 阈值与 `mem::take` 取段的依据。
//!
//! 运行：
//! `cargo test -p peri-tui --release --lib append_probe -- --ignored --nocapture`

use crate::kit::tui_render_unit::{FoldState, TuiRenderUnit, TuiToolCard, TuiToolPresentation};

fn make_unit(i: usize) -> TuiRenderUnit {
    TuiRenderUnit::TuiToolCard(TuiToolCard {
        tool_id: format!("t{i}"),
        tool_name: "Bash".into(),
        input_summary: "echo hi".into(),
        output_summary: "ok".into(),
        is_error: false,
        is_running: false,
        running_duration_ms: None,
        completed_duration_ms: Some(37),
        diff: None,
        presentation: TuiToolPresentation::Generic,
        fold: FoldState::Expanded,
        user_modified: false,
        content_hash: i as u64,
        tool_calls_count: 0,
    })
}

fn base(n: usize) -> im::Vector<TuiRenderUnit> {
    (0..n).map(make_unit).collect()
}

#[test]
#[ignore = "定向测量，手动运行：cargo test -p peri-tui --release --lib append_probe -- --ignored --nocapture"]
fn append_probe_split_cost() {
    let n = 1000usize;
    let b = base(n);
    let iters = 200usize;
    for k in [0usize, 1, 8, 500, 992, 999, 1000] {
        // split_off(k)：保留 [0,k)，返回 [k..)
        let start = std::time::Instant::now();
        for _ in 0..iters {
            let mut v = b.clone();
            let tail = v.split_off(k);
            std::hint::black_box((&v, &tail));
        }
        let split_us = start.elapsed().as_secs_f64() * 1e6 / iters as f64;

        // slice(k..)：等价取后缀，但实现是 2 次 split_off + append
        let start = std::time::Instant::now();
        for _ in 0..iters {
            let mut v = b.clone();
            let tail = v.slice(k..);
            std::hint::black_box((&v, &tail));
        }
        let slice_us = start.elapsed().as_secs_f64() * 1e6 / iters as f64;

        // Focus 窗口读取（零拷贝视图）：顺序取 [k..) 全部元素
        let start = std::time::Instant::now();
        for _ in 0..iters {
            let f = b.focus();
            let mut g = f.narrow(k..);
            let mut acc = 0usize;
            for i in 0..g.len() {
                acc += g.get(i).map(|_| 1).unwrap_or(0);
            }
            std::hint::black_box(acc);
        }
        let focus_us = start.elapsed().as_secs_f64() * 1e6 / iters as f64;

        println!(
            "SPLIT k={k:>4} split_off={split_us:>8.2} slice={slice_us:>8.2} \
             focus_scan={focus_us:>8.2}  (us, base={n})"
        );
    }
}

#[test]
#[ignore = "定向测量，手动运行：cargo test -p peri-tui --release --lib append_probe -- --ignored --nocapture"]
fn append_probe_vector_join() {
    println!(
        "size_of::<TuiRenderUnit>() = {}",
        std::mem::size_of::<TuiRenderUnit>()
    );
    for n in [200usize, 1000] {
        let b = base(n);
        println!("--- base={n} ---");
        for tail in [1usize, 4, 32, 256] {
            let t: im::Vector<TuiRenderUnit> = (0..tail).map(|i| make_unit(n + i)).collect();
            let iters = 200usize;

            let start = std::time::Instant::now();
            for _ in 0..iters {
                let mut v = b.clone();
                v.append(t.clone());
                std::hint::black_box(&v);
            }
            let append_us = start.elapsed().as_secs_f64() * 1e6 / iters as f64;

            let start = std::time::Instant::now();
            for _ in 0..iters {
                let mut v = b.clone();
                for x in t.iter() {
                    v.push_back(x.clone());
                }
                std::hint::black_box(&v);
            }
            let push_us = start.elapsed().as_secs_f64() * 1e6 / iters as f64;

            let start = std::time::Instant::now();
            for _ in 0..iters {
                let v = b.clone();
                std::hint::black_box(&v);
            }
            let clone_us = start.elapsed().as_secs_f64() * 1e6 / iters as f64;

            let start = std::time::Instant::now();
            for _ in 0..iters {
                let mut v = b.clone();
                v.split_off(n / 2);
                std::hint::black_box(&v);
            }
            let split_us = start.elapsed().as_secs_f64() * 1e6 / iters as f64;

            println!(
                "APPEND tail={tail:>4} clone={clone_us:>7.2} append={append_us:>8.2} \
                 push_back={push_us:>8.2} split_off_half={split_us:>7.2}  (us)"
            );
        }
    }
}
