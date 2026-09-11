//! 前置扫描实验：输入、语义评估和终端报告各有唯一入口。
//!
//! 在本目录运行 `cargo run`；未预期失败或缺失实验组返回非零退出码。

mod evaluation;
mod matrix;
mod report;
mod scanner;

fn main() -> std::process::ExitCode {
    let experiment = evaluation::run();
    report::print(&experiment);
    std::process::ExitCode::from(experiment.exit_code())
}
