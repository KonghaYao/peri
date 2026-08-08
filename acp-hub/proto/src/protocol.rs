//! §16 协议参数默认值（供 server config 引用为默认；server 可覆盖）。
//!
//! 仅承载**协议参数**。其余 §16 项（监听地址/端口、数据目录、命令队列 64、
//! 连接配额 200、背压 64KB/128KB、微批次 16ms、超时组、fsync、compact、
//! 磁盘预算、归档、env 白名单、allow_non_loopback）属 server 运维配置，
//! 定义在 `server/src/config`，不在 proto（设计文档 §1 边界声明）。

use std::time::Duration;

/// §16 协议参数默认值（常量集合）。
pub struct Defaults;

impl Defaults {
    /// 心跳间隔（§16 / §7.1：默认 5s）。
    pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

    /// 离线判定超时（§16 / §7.1：默认 30s）。
    pub const OFFLINE_TIMEOUT: Duration = Duration::from_secs(30);

    /// 缓冲环形滑窗条数（§16 / §8.5：最后 500 条，覆盖 server 崩溃前已收未落盘段）。
    pub const RING_BUFFER_CAPACITY: usize = 500;

    /// 单帧大小上限（§16 / §8.5：1MB）。
    pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

    /// 缓冲上限（内存 + 磁盘合计，§16 / §8.5：10MB）。
    pub const BUFFER_LIMIT_BYTES: usize = 10 * 1024 * 1024;

    /// 缓冲上限（条数，§16 / §8.5：万条）。
    pub const BUFFER_LIMIT_FRAMES: usize = 10_000;
}
