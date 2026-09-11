//! 终端报告只消费评估结果，不定义实验成功条件。

use crate::evaluation::{Check, Experiment};

pub fn print(experiment: &Experiment) {
    println!("=== md-scan-matrix: 前置扫描 + 占位 token 替换实验 ===\n");
    println!("## 矩阵 A–E/G\n");
    for o in &experiment.matrix {
        println!(
            "{} | {} | hits={}/{} 切片={} NUL结构={} PUA结构={} NUL计数={} PUA计数={} 无残留={}",
            verdict(o.pass(), o.expected_collision),
            o.name,
            o.actual_hits,
            o.expect_hits,
            o.slice_ok,
            o.shape_nul,
            o.shape_pua,
            o.nul_retained,
            o.pua_retained,
            o.leftover_ok
        );
        println!("    {}；切片={:?}", o.note, o.slices);
        if !o.hit_summary.is_empty() {
            println!("    {}", o.hit_summary);
        }
        for detail in &o.detail {
            println!("    {detail}");
        }
    }
    print_checks("F 流式序列与已闭合前缀", &experiment.streaming);
    print_checks("G 碰撞负例与重编号", &experiment.collisions);
    print_checks("H reference 解析", &experiment.references);
    println!(
        "\n结论: {}；矩阵 {} cases，补充 {} checks，未预期失败 {}；exit={}",
        if experiment.exit_code() == 0 {
            "PASS"
        } else {
            "FAIL"
        },
        experiment.matrix.len(),
        experiment.streaming.len() + experiment.collisions.len() + experiment.references.len(),
        experiment.failures(),
        experiment.exit_code()
    );
}

fn print_checks(title: &str, checks: &[Check]) {
    println!("\n## {title}\n");
    for check in checks {
        println!(
            "{} | {} | {}",
            verdict(check.passed, check.expected_collision),
            check.name,
            check.detail
        );
    }
}

fn verdict(passed: bool, expected_collision: bool) -> &'static str {
    match (passed, expected_collision) {
        (false, _) => "FAIL",
        (true, true) => "EXPECTED COLLISION",
        (true, false) => "PASS",
    }
}
