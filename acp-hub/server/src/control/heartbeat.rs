//! keep_alive 心跳（架构 §4.7，设计稿 `f5-channel-control.md` §13）。
//!
//! server 每 `interval`（默认 5s）下发 `keep_alive`；pong 超时
//! （`timeout`，默认 3×interval = 15s【决策】：文档仅定义「超时未回以 4501
//! 关闭」，未给判定时长；与 machine 离线 30s 解耦——keep_alive 只测连接
//! 活性，不判机器）→ 关闭码 4501。
//!
//! gateway 主循环以 [`HeartbeatDriver`] 状态机集成（select 循环内驱动，
//! 避免双消费者竞争入站 receiver）；[`Heartbeat::run_for`] 提供独立 task
//! 形态（测试/纯心跳连接用）。

use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use acp_hub_proto::conn::KeepAlive;
use acp_hub_proto::frame::Frame;

use crate::control::close_codes::CLOSE_KEEPALIVE_TIMEOUT;

/// 心跳判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeartbeatOutcome {
    /// 正常结束（receiver 关闭 / 连接关闭）。
    Ok,
    /// pong 超时 → 以 4501 关闭（§4.7）。
    Timeout(u16),
    /// 出站通道关闭。
    ChannelClosed,
}

/// 心跳参数（§16 默认 5s 间隔；pong 超时【决策】3×interval）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Heartbeat {
    /// keep_alive 下发间隔（§16 默认 5s）。
    pub interval: Duration,
    /// pong 超时（【决策】默认 3×interval；超时未回 → 4501）。
    pub timeout: Duration,
}

impl Heartbeat {
    /// 以间隔构造（超时 = 3×interval，设计稿 §13【决策】）。
    pub fn new(interval: Duration) -> Self {
        Heartbeat {
            interval,
            timeout: interval * 3,
        }
    }

    /// 每连接心跳 task（独立形态；gateway 主循环用 [`HeartbeatDriver`] 集成）。
    ///
    /// 周期下发 `keep_alive`；`recv` 中匹配 `pong` 续期；超时 → `Timeout(4501)`。
    pub async fn run_for(
        &self,
        send: mpsc::Sender<Frame>,
        recv: &mut mpsc::Receiver<Frame>,
    ) -> HeartbeatOutcome {
        let mut driver = HeartbeatDriver::new(self.interval, self.timeout);
        let mut ticker = tokio::time::interval(self.interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // 首个 tick 立即就绪：消费掉让窗口从此刻计时。
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if driver.should_send_keepalive(Instant::now())
                        && send.send(Frame::KeepAlive(KeepAlive {})).await.is_err()
                    {
                        return HeartbeatOutcome::ChannelClosed;
                    }
                    if driver.check_timeout(Instant::now()) {
                        return HeartbeatOutcome::Timeout(CLOSE_KEEPALIVE_TIMEOUT);
                    }
                }
                frame = recv.recv() => {
                    match frame {
                        Some(Frame::Pong(_)) => driver.on_pong(),
                        Some(_) => { /* 非 pong 帧由主循环消费；独立形态下忽略 */ }
                        None => return HeartbeatOutcome::Ok,
                    }
                }
            }
        }
    }
}

/// 心跳状态机（无 I/O；gateway 主循环集成）。
#[derive(Debug, Clone, Copy)]
pub struct HeartbeatDriver {
    interval: Duration,
    timeout: Duration,
    /// 上次 pong 时刻（或连接建立时刻）。
    last_pong: Instant,
    /// 上次 keep_alive 下发时刻。
    last_sent: Instant,
}

impl HeartbeatDriver {
    /// 新建（`now` 以 [`Instant::now`] 为基准；测试可注入虚拟时钟推进）。
    ///
    /// `last_sent` 初始化为 `now - interval`：连接建立即视为已到期，首次
    /// 检查（如首个 tick）立即下发一次 `keep_alive`（§4.7 连接活性从
    /// 建立时刻即受监控）。
    pub fn new(interval: Duration, timeout: Duration) -> Self {
        let now = Instant::now();
        HeartbeatDriver {
            interval,
            timeout,
            last_pong: now,
            // `checked_sub`：interval 大于进程运行时长（理论极端）时回退 now
            // ——首次检查仍视为到期（diff = 0 < interval 时下个周期自然触发）。
            last_sent: now.checked_sub(interval).unwrap_or(now),
        }
    }

    /// pong 回执续期。
    pub fn on_pong(&mut self) {
        self.last_pong = Instant::now();
    }

    /// 是否到 keep_alive 下发时刻（距上次下发 ≥ interval）。
    pub fn should_send_keepalive(&self, now: Instant) -> bool {
        now.duration_since(self.last_sent) >= self.interval
    }

    /// 记录一次 keep_alive 下发（调用方在发送成功后调用）。
    pub fn note_sent(&mut self) {
        self.last_sent = Instant::now();
    }

    /// pong 超时判定（距上次 pong ≥ timeout → true，关闭码 4501）。
    pub fn check_timeout(&self, now: Instant) -> bool {
        now.duration_since(self.last_pong) >= self.timeout
    }
}

#[cfg(test)]
mod heartbeat_test {
    use super::*;

    #[test]
    fn timeout_three_times_interval() {
        let hb = Heartbeat::new(Duration::from_secs(5));
        assert_eq!(hb.timeout, Duration::from_secs(15));
    }

    #[test]
    fn driver_pong_extends_window() {
        let mut d = HeartbeatDriver::new(
            Duration::from_secs(5),
            Duration::from_secs(15),
        );
        let t0 = Instant::now();
        // 14s 未 pong：未超时。
        assert!(!d.check_timeout(t0 + Duration::from_secs(14)));
        // 16s：超时。
        assert!(d.check_timeout(t0 + Duration::from_secs(16)));
        // pong 续期后重置。
        d.on_pong();
        assert!(!d.check_timeout(Instant::now()));
        assert!(!d.check_timeout(Instant::now() + Duration::from_secs(14)));
        assert!(d.check_timeout(Instant::now() + Duration::from_secs(16)));
    }

    #[test]
    fn driver_keepalive_period() {
        let mut d = HeartbeatDriver::new(Duration::from_secs(5), Duration::from_secs(15));
        let t0 = Instant::now();
        // 创建即视为到期（last_sent = 创建时刻 - interval）→ 首次检查应发。
        assert!(d.should_send_keepalive(t0));
        d.note_sent();
        // note_sent 后 last_sent = 真实 now（≥ t0）；以真实 now 为基准避免
        // 时钟微差。
        let base = Instant::now();
        assert!(!d.should_send_keepalive(base + Duration::from_secs(4)));
        assert!(d.should_send_keepalive(base + Duration::from_secs(5)));
    }
}
