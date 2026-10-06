//! 临时性能探针：测量真实 WorkState 样本的解码 / 克隆 / 编码成本。
//! 仅用于 P0 CPU/内存根因评估，验证完成后删除。

use peri_acp_types::session_resources::work::WorkState;
use std::time::Instant;

#[test]
#[ignore = "manual perf probe"]
fn probe_work_state_costs() {
    let path = std::env::var("PERI_PROBE_STATE").expect("PERI_PROBE_STATE not set");
    let json = std::fs::read_to_string(&path).expect("state sample is readable");
    println!("sample_bytes={}", json.len());

    let t = Instant::now();
    let state: WorkState = serde_json::from_str(&json).expect("sample decodes");
    println!("decode={:?}", t.elapsed());

    let t = Instant::now();
    let cloned = state.clone();
    println!("clone_cold={:?}", t.elapsed());

    let t = Instant::now();
    let cloned2 = state.clone();
    println!("clone_warm={:?}", t.elapsed());

    let t = Instant::now();
    let encoded = serde_json::to_string(&state).expect("sample encodes");
    println!("encode={:?}", t.elapsed());
    println!("encoded_bytes={}", encoded.len());

    std::hint::black_box((cloned, cloned2, encoded));
}
