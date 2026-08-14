//! 断线缓冲（§8.3/§8.5）：per-session 分桶（内存 + 磁盘两级）、分类丢弃、
//! 环形滑窗 500、水位文件（epoch/last_seq/pgid）、启动清理。
//!
//! - **两级存储**：内存 `VecDeque` 优先（预算 `mem_buffer_bytes` 与条数上限的
//!   一半）；达限后新帧追加磁盘 append 日志（`{data_dir}/buffer/{sid}.buf`，
//!   0600，u32 长度前缀 + [`BufferedFrame`] 序列化，无 CRC【决策】——崩溃即弃
//!   的临时溢出文件，§3.3「缓冲不跨重启保留」）；
//! - **预算与丢弃**（§8.5）：总预算 = 内存 + 磁盘合计（10MB/万条默认，任一超限
//!   触发）：单帧超限跳过（gap）；超预算事件类优先丢弃、控制类最后丢弃；
//!   分类规则为**信封结构性分类**【决策】（instance 是 dumb pipe，§3.3 禁止
//!   语义解析）：JSON-RPC 包裹且含 `id` → 控制类；通知/原始帧 → 事件类；
//! - **补推**：`drain_batch`（peek，不移除）→ 发送成功 → `commit`（移除）；
//!   发送中断 → `rollback`（帧回置队首）——保证「未确认不移出」；
//! - **环形滑窗**：每 session 常驻内存最后 500 条（在线与断线均写入），兜底
//!   server 崩溃前已收未落盘段（`ring_snapshot` 查询接口备用，冲突 2）；
//! - **水位**：`{data_dir}/watermark.json`（0600）：epoch 跨重启单调（§4.5.1
//!   判定正确性前提）、pgid + leader 出生指纹 + data-dir 身份供可证明
//!   所有权的启动清理，last_seq 仅作诊断参考（权威在 server）。

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use acp_hub_proto::instance::BufferedFrame;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------------------------------------------------------------------------
// 帧分类（§4.4.1 第 3 条：信封结构性分类【决策】）
// ---------------------------------------------------------------------------

/// 帧分类（丢弃优先级依据，§8.5「delta 类帧优先丢弃、控制帧/终态帧最后丢弃」
/// 的 instance 侧可执行近似）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// 事件类（无 `id` 的通知 / 原始 `{type,payload}` 帧）——优先丢弃。
    Event,
    /// 控制类（JSON-RPC 请求/响应，含 `"jsonrpc"` 键且有 `"id"`）——最后丢弃。
    Control,
}

/// 信封结构性分类（不解析事件语义，§3.3）：
/// JSON-RPC 包裹（含 `"jsonrpc"` 键）且有 `"id"` → 控制类；其余 → 事件类。
pub fn classify_frame(frame: &serde_json::Value) -> FrameKind {
    if frame.get("jsonrpc").is_some() && frame.get("id").is_some() {
        FrameKind::Control
    } else {
        FrameKind::Event
    }
}

// ---------------------------------------------------------------------------
// 磁盘段
// ---------------------------------------------------------------------------

/// 磁盘溢出段：append 日志（`u32 BE 长度 + BufferedFrame JSON`），0600。
///
/// 写句柄与读句柄独立（写后 flush 保证同进程可见）；「从段首丢弃」= 推进跳过
/// 游标（append-only，不重写文件）。
struct DiskSegment {
    path: PathBuf,
    writer: BufWriter<File>,
    reader: Option<BufReader<File>>,
    /// 文件内全部记录数（含已跳过）。
    records: u64,
    /// 已从段首跳过的记录数（丢弃/commit 消费）。
    skip_records: u64,
    /// 已跳过字节数（读游标偏移）。
    skip_bytes: usize,
}

impl DiskSegment {
    fn open(dir: &Path, chat_id: &str) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{chat_id}.buf"));
        let f = OpenOptions::new().create(true).append(true).open(&path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = f.set_permissions(fs::Permissions::from_mode(0o600));
        }
        Ok(DiskSegment {
            path,
            writer: BufWriter::new(f),
            reader: None,
            records: 0,
            skip_records: 0,
            skip_bytes: 0,
        })
    }

    /// 追加一条记录并 flush（读句柄可见性 + 崩溃即弃语义）。
    fn append(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let len = u32::try_from(bytes.len())
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "帧过长（>4GB）"))?;
        self.writer.write_all(&len.to_be_bytes())?;
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        self.records += 1;
        Ok(())
    }

    /// 确保读句柄打开并定位到跳过游标。
    fn ensure_reader(&mut self) -> std::io::Result<&mut BufReader<File>> {
        if self.reader.is_none() {
            let f = File::open(&self.path)?;
            let r = BufReader::new(f);
            self.reader = Some(r);
        }
        let r = self.reader.as_mut().expect("reader 已初始化");
        r.seek(SeekFrom::Start(self.skip_bytes as u64))?;
        Ok(r)
    }

    /// 读取段首一条记录（不消费；无记录 → None）。`out_len` 记录字节数。
    fn peek_one(&mut self, out_len: &mut usize) -> std::io::Result<Option<BufferedFrame>> {
        if self.skip_records >= self.records {
            return Ok(None);
        }
        let r = self.ensure_reader()?;
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf)?;
        let len = u32::from_be_bytes(len_buf) as usize;
        let mut body = vec![0u8; len];
        r.read_exact(&mut body)?;
        let bf: BufferedFrame = serde_json::from_slice(&body).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("缓冲文件损坏: {e}"),
            )
        })?;
        *out_len = 4 + len;
        Ok(Some(bf))
    }

    /// 从段首消费（跳过）一条记录：返回 (帧, 字节数) 用于分类计数。
    fn consume_one(&mut self) -> std::io::Result<Option<(BufferedFrame, usize)>> {
        let mut len = 0;
        match self.peek_one(&mut len)? {
            Some(bf) => {
                self.skip_records += 1;
                self.skip_bytes += len;
                Ok(Some((bf, len)))
            }
            None => Ok(None),
        }
    }
}

// ---------------------------------------------------------------------------
// SessionBuffer
// ---------------------------------------------------------------------------

/// 单 session 分桶缓冲（内存段 + 磁盘段 + 丢弃计数 + 补推 in-flight）。
pub struct SessionBuffer {
    pub chat_id: String,
    mem: VecDeque<(BufferedFrame, usize)>,
    mem_bytes: usize,
    disk: Option<DiskSegment>,
    /// 总有效字节（mem + disk 未跳过段，push/consume 同步核算）。
    total_bytes: usize,
    /// 总有效帧数（mem + disk 未跳过段）。
    total_frames: usize,
    /// 补推 in-flight（已 drain 未 commit；发送失败 rollback 回置）。
    in_flight: VecDeque<(BufferedFrame, usize)>,
    dropped_event: u64,
    dropped_control: u64,
    dropped_oversize: u64,
}

impl SessionBuffer {
    fn new(chat_id: &str) -> Self {
        SessionBuffer {
            chat_id: chat_id.to_string(),
            mem: VecDeque::new(),
            mem_bytes: 0,
            disk: None,
            total_bytes: 0,
            total_frames: 0,
            in_flight: VecDeque::new(),
            dropped_event: 0,
            dropped_control: 0,
            dropped_oversize: 0,
        }
    }

    fn first_seq(&mut self) -> Option<u64> {
        // from_seq = **本批**首帧 seq（调用方按批序发送并 commit，in-flight
        // 帧先于本批离流，不计入起点——见 drain_batch 契约与 buffer_test）。
        if let Some((bf, _)) = self.mem.front() {
            return Some(bf.seq);
        }
        if let Some((bf, _)) = self.in_flight.front() {
            return Some(bf.seq);
        }
        // 磁盘段首帧 seq 需读取（低频路径）。
        self.disk
            .as_mut()
            .and_then(|d| {
                let mut len = 0;
                d.peek_one(&mut len).ok().flatten()
            })
            .map(|bf| bf.seq)
    }

    /// 丢弃一条（预算超限）：内存段优先「最旧事件帧」，无事件帧则最旧帧；
    /// 内存空 → 磁盘段首（append-only 限制，最旧优先【决策】，见模块文档）。
    fn evict_one(&mut self) -> bool {
        if !self.mem.is_empty() {
            if let Some(pos) = self
                .mem
                .iter()
                .position(|(bf, _)| classify_frame(&bf.frame) == FrameKind::Event)
            {
                let (_, len) = self.mem.remove(pos).expect("位置已检查");
                self.mem_bytes -= len;
                self.total_bytes -= len;
                self.total_frames -= 1;
                self.dropped_event += 1;
                return true;
            }
            let (_, len) = self.mem.pop_front().expect("mem 非空");
            self.mem_bytes -= len;
            self.total_bytes -= len;
            self.total_frames -= 1;
            self.dropped_control += 1;
            return true;
        }
        if let Some(disk) = self.disk.as_mut() {
            match disk.consume_one() {
                Ok(Some((bf, len))) => {
                    self.total_bytes -= len;
                    self.total_frames -= 1;
                    if classify_frame(&bf.frame) == FrameKind::Event {
                        self.dropped_event += 1;
                    } else {
                        self.dropped_control += 1;
                    }
                    return true;
                }
                _ => return false,
            }
        }
        false
    }
}

/// 单帧超限 / 正常缓冲的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOutcome {
    /// 已入缓冲（或入缓冲后被预算丢弃——丢弃计数在 buffer 内）。
    Buffered,
    /// 序列化后超单帧上限：不入缓冲、不转发，跳过 + gap 计数（§8.5）。
    Oversize,
}

// ---------------------------------------------------------------------------
// Buffer（聚合）
// ---------------------------------------------------------------------------

/// 全局缓冲：`chat_id → SessionBuffer` 分桶 + 预算核算（§8.5 合计口径）。
pub struct Buffer {
    chats: HashMap<String, SessionBuffer>,
    mem_bytes_limit: usize,
    mem_frames_limit: usize,
    total_bytes_limit: usize,
    total_frames_limit: usize,
    max_frame_bytes: usize,
    dir: PathBuf,
}

impl Buffer {
    /// 构建缓冲池。
    ///
    /// - `mem_bytes_limit`：内存段字节预算（默认 5MB = 合计口径的内存半区【决策】）；
    /// - `mem_frames_limit`：内存段条数预算（默认合计上限的一半【决策】）；
    /// - `total_bytes_limit` / `total_frames_limit`：内存 + 磁盘合计（默认 10MB/万条）；
    /// - `max_frame_bytes`：单帧上限（超限跳过 + gap，§8.5）；
    /// - `dir`：磁盘溢出文件目录（`{data_dir}/buffer/`）。
    pub fn new(
        mem_bytes_limit: usize,
        mem_frames_limit: usize,
        total_bytes_limit: usize,
        total_frames_limit: usize,
        max_frame_bytes: usize,
        dir: PathBuf,
    ) -> Self {
        Buffer {
            chats: HashMap::new(),
            mem_bytes_limit,
            mem_frames_limit,
            total_bytes_limit,
            total_frames_limit,
            max_frame_bytes,
            dir,
        }
    }

    /// 入缓冲（断线路径）。单帧超限 → [`PushOutcome::Oversize`]（跳过 + gap，
    /// seq 不消耗——调用方按 `Oversize` 处理缺口计数）。
    pub fn push(&mut self, chat_id: &str, seq: u64, frame: serde_json::Value) -> PushOutcome {
        let entry = self
            .chats
            .entry(chat_id.to_string())
            .or_insert_with(|| SessionBuffer::new(chat_id));
        let bf = BufferedFrame { seq, frame };
        let bytes = match serde_json::to_vec(&bf) {
            Ok(b) => b,
            Err(_) => return PushOutcome::Oversize, // 理论不可达（Value 恒可序列化）
        };
        if bytes.len() > self.max_frame_bytes {
            entry.dropped_oversize += 1;
            tracing::warn!(target: "acp_hub::instance", chat_id, seq, bytes = bytes.len(),
                max = self.max_frame_bytes, "缓冲帧超单帧上限，跳过（gap）");
            return PushOutcome::Oversize;
        }
        let size = bytes.len();
        if entry.mem_bytes + size <= self.mem_bytes_limit && entry.mem.len() < self.mem_frames_limit
        {
            entry.mem_bytes += size;
            entry.mem.push_back((bf, size));
        } else {
            // 内存段满 → 磁盘溢出段（懒创建 0600）。
            if entry.disk.is_none() {
                match DiskSegment::open(&self.dir, chat_id) {
                    Ok(d) => entry.disk = Some(d),
                    Err(e) => {
                        tracing::error!(target: "acp_hub::instance", chat_id,
                            "缓冲磁盘段打开失败: {e}");
                        return PushOutcome::Oversize;
                    }
                }
            }
            if let Some(disk) = entry.disk.as_mut() {
                if let Err(e) = disk.append(&bytes) {
                    tracing::error!(target: "acp_hub::instance", chat_id,
                        "缓冲磁盘段写入失败: {e}");
                    return PushOutcome::Oversize;
                }
            }
        }
        entry.total_bytes += size;
        entry.total_frames += 1;

        // 预算丢弃（§8.5：内存 + 磁盘合计口径，任一超限触发）。
        while entry.total_bytes > self.total_bytes_limit
            || entry.total_frames > self.total_frames_limit
        {
            if !entry.evict_one() {
                break;
            }
        }
        PushOutcome::Buffered
    }

    /// 该 session 是否有待补推帧（pending = 有效帧，不含 in-flight）。
    pub fn has_pending(&self, chat_id: &str) -> bool {
        self.chats
            .get(chat_id)
            .map(|e| e.total_frames > 0)
            .unwrap_or(false)
    }

    /// 任一 session 有待补推帧（`hello.buffered`，§6.3）。
    pub fn has_any_pending(&self) -> bool {
        self.chats.values().any(|e| e.total_frames > 0)
    }

    /// 补推批次：从 pending 首部（内存优先，跨磁盘段）取最多 `max_frames` 帧、
    /// 合计序列化 ≤ `max_bytes` 字节（§6.2 分批【决策】：256 帧 / 512KB 先达者）。
    ///
    /// **peek 语义**：帧移入 in-flight 但未消费；发送成功须 [`Buffer::commit`]，
    /// 失败须 [`Buffer::rollback`]——保证「未确认不移出」（§6.1/§6.2）。
    ///
    /// 返回 `(from_seq, frames)`；无 pending → `None`。
    pub fn drain_batch(
        &mut self,
        chat_id: &str,
        max_frames: usize,
        max_bytes: usize,
    ) -> Option<(u64, Vec<BufferedFrame>)> {
        let entry = self.chats.get_mut(chat_id)?;
        if entry.total_frames == 0 {
            return None;
        }
        let from_seq = entry.first_seq()?;
        let mut out = Vec::new();
        let mut bytes = 0usize;
        while out.len() < max_frames {
            // 内存段优先（队首即全局最旧）。
            if let Some((_, len)) = entry.mem.front() {
                if bytes + *len > max_bytes {
                    break;
                }
                let (bf, len) = entry.mem.pop_front().expect("front 已检查");
                entry.mem_bytes -= len;
                bytes += len;
                out.push((bf, len));
                continue;
            }
            // 内存空 → 磁盘段（body 字节口径与内存一致：不含 4B 长度前缀）。
            if let Some(disk) = entry.disk.as_mut() {
                let mut len = 0;
                match disk.peek_one(&mut len) {
                    Ok(Some(bf)) => {
                        let body_len = len.saturating_sub(4);
                        if bytes + body_len > max_bytes {
                            break;
                        }
                        disk.consume_one().ok()?;
                        bytes += body_len;
                        out.push((bf, body_len));
                        continue;
                    }
                    _ => break,
                }
            } else {
                break;
            }
        }
        if out.is_empty() {
            return None;
        }
        // 移入 in-flight（顺序保持；未确认不移出，§6.1）。
        entry.in_flight.extend(out.iter().cloned());
        Some((from_seq, out.into_iter().map(|(bf, _)| bf).collect()))
    }

    /// 确认补推批次已发送成功：in-flight 帧正式出流（该 session 补推串行，
    /// 一次 commit 清空整批）。
    pub fn commit(&mut self, chat_id: &str) {
        if let Some(entry) = self.chats.get_mut(chat_id) {
            let mut drained_bytes = 0usize;
            let mut drained_frames = 0usize;
            while let Some((_, len)) = entry.in_flight.pop_front() {
                drained_bytes += len;
                drained_frames += 1;
            }
            entry.total_bytes = entry.total_bytes.saturating_sub(drained_bytes);
            entry.total_frames = entry.total_frames.saturating_sub(drained_frames);
        }
    }

    /// 补推发送中断（断线）：in-flight 帧回置 pending 队首（顺序保持，§6.2
    /// 「未发帧保留在 pending，重连后重发，from_seq 不变」）。
    pub fn rollback(&mut self, chat_id: &str) {
        if let Some(entry) = self.chats.get_mut(chat_id) {
            let frames: Vec<_> = entry.in_flight.drain(..).collect();
            for (bf, len) in frames.into_iter().rev() {
                entry.mem.push_front((bf, len));
                entry.mem_bytes += len;
            }
        }
    }

    /// 全部分桶回置（resync 任务被中断时调用——abort 可能发生在持锁/发送
    /// 之间，孤儿 in-flight 帧必须回置 pending，否则重连后 from_seq 错位）。
    pub fn rollback_all(&mut self) {
        let ids: Vec<String> = self
            .chats
            .iter()
            .filter(|(_, e)| !e.in_flight.is_empty())
            .map(|(sid, _)| sid.clone())
            .collect();
        for sid in ids {
            self.rollback(&sid);
        }
    }

    /// 会话清理：删除缓冲文件与内存段（§8.5「session 结束/清理时同步删除」）。
    pub fn remove(&mut self, chat_id: &str) {
        if let Some(entry) = self.chats.remove(chat_id) {
            if let Some(disk) = entry.disk {
                let _ = fs::remove_file(&disk.path);
            }
        }
    }

    /// 清空全部分桶（daemon 启动时，§3.3 缓冲不跨重启）+ 删除目录内文件。
    pub fn clear_all(&mut self) {
        let dir = self.dir.clone();
        self.chats.clear();
        let _ = fs::remove_dir_all(&dir);
    }

    /// 丢弃计数合计（§17.1 指标：事件/控制/超限分类）。
    pub fn dropped_stats(&self) -> (u64, u64, u64) {
        let mut e = 0;
        let mut c = 0;
        let mut o = 0;
        for s in self.chats.values() {
            e += s.dropped_event;
            c += s.dropped_control;
            o += s.dropped_oversize;
        }
        (e, c, o)
    }

    /// 缓冲水位（字节/条数合计，§17.1 指标）。
    pub fn water_level(&self) -> (usize, usize) {
        let mut bytes = 0;
        let mut frames = 0;
        for s in self.chats.values() {
            bytes += s.total_bytes;
            frames += s.total_frames;
        }
        (bytes, frames)
    }
}

// ---------------------------------------------------------------------------
// 环形滑窗（§4.4.2）
// ---------------------------------------------------------------------------

/// 环形滑窗：常驻内存最后 `cap` 条（在线 = 已发送帧；断线 = 缓冲帧）。
///
/// 兜底「server 崩溃前已收未落盘段」：server 发现缺口时请求滑窗重发
/// （冲突 2 无线帧，本类型仅提供查询接口备用）。满则淘汰最旧。
#[derive(Debug, Clone)]
pub struct RingBuffer {
    frames: VecDeque<BufferedFrame>,
    cap: usize,
}

impl RingBuffer {
    pub fn new(cap: usize) -> Self {
        RingBuffer {
            frames: VecDeque::new(),
            cap,
        }
    }

    /// 入窗（满则淘汰最旧）。
    pub fn push(&mut self, bf: BufferedFrame) {
        if self.frames.len() == self.cap {
            self.frames.pop_front();
        }
        self.frames.push_back(bf);
    }

    /// 快照（seq 升序，备用查询接口）。
    pub fn snapshot(&self) -> Vec<BufferedFrame> {
        self.frames.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

// ---------------------------------------------------------------------------
// 水位文件（§4.4.3）
// ---------------------------------------------------------------------------

/// 水位文件错误。
#[derive(Debug, Error)]
pub enum WatermarkError {
    #[error("水位文件 I/O 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("水位文件解析错误: {0}")]
    Parse(#[from] serde_json::Error),
}

/// 水位文件 JSON 形态（§4.4.3）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatermarkFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir_identity: Option<DataDirIdentity>,
    pub chats: HashMap<String, SessionWatermark>,
}

/// Unix data-dir identity. A copied directory must not inherit process ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataDirIdentity {
    pub device: u64,
    pub inode: u64,
}

/// Opaque platform process birth identity for the process-group leader.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessFingerprint {
    pub platform: String,
    pub birth: String,
}

/// 单 session 水位（§4.4.3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionWatermark {
    /// 流纪元（§4.5.1）。
    pub epoch: u64,
    /// 诊断参考 last_seq（非权威，权威在 server 侧 f3-persist 水位）。
    pub last_seq: u64,
    /// 进程组 id（启动清理残留用；0 = 无）。
    pub pgid: i32,
    /// Exact leader birth identity. Missing means cleanup authority is unproven.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_fingerprint: Option<ProcessFingerprint>,
}

/// 水位存储：epoch 跨重启单调 + 可验证的进程所有权（§4.4.3）。
///
/// 写盘用临时文件 + rename（原子，0600）；「last_seq 高频不落盘」由调用方
/// 控制 record 频率（epoch 变更时才写）。
#[derive(Debug)]
pub struct Watermark {
    path: PathBuf,
    state: WatermarkFile,
}

impl Watermark {
    /// 加载（文件不存在 → 空水位）。
    pub fn load(data_dir: &Path) -> Result<Self, WatermarkError> {
        let path = data_dir.join("watermark.json");
        let state = match fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
                tracing::warn!(target: "acp_hub::instance", path = %path.display(),
                    "水位文件损坏，按空水位处理: {e}");
                WatermarkFile::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => WatermarkFile::default(),
            Err(e) => return Err(WatermarkError::Io(e)),
        };
        Ok(Watermark { path, state })
    }

    /// 某 session 的水位 epoch（无记录 → None）。
    pub fn epoch_of(&self, chat_id: &str) -> Option<u64> {
        self.state.chats.get(chat_id).map(|s| s.epoch)
    }

    /// Runtime ownership records retained for startup cleanup.
    pub fn runtime_records(&self) -> Vec<(i32, Option<ProcessFingerprint>)> {
        self.state
            .chats
            .values()
            .filter(|s| s.pgid > 0)
            .map(|s| (s.pgid, s.process_fingerprint.clone()))
            .collect()
    }

    pub fn data_dir_identity(&self) -> Option<DataDirIdentity> {
        self.state.data_dir_identity
    }

    /// Consume all stale cleanup authority while preserving epoch/sequence history.
    pub fn finalize_startup(&mut self, identity: DataDirIdentity) -> Result<(), WatermarkError> {
        self.state.data_dir_identity = Some(identity);
        for session in self.state.chats.values_mut() {
            session.pgid = 0;
            session.process_fingerprint = None;
        }
        self.write()
    }

    /// 写盘：临时文件（0600）+ rename（原子，§4.4.3【决策】）。
    pub fn write(&self) -> Result<(), WatermarkError> {
        let content = serde_json::to_string_pretty(&self.state)?;
        let tmp = self.path.with_extension("json.tmp");
        {
            let mut f = File::create(&tmp)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                f.set_permissions(fs::Permissions::from_mode(0o600))?;
            }
            f.write_all(content.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// 更新（或新增）session 水位并落盘（epoch 变更时调用，§4.4.3 更新时机）。
    pub fn record(
        &mut self,
        chat_id: &str,
        epoch: u64,
        last_seq: u64,
        pgid: i32,
        process_fingerprint: Option<ProcessFingerprint>,
    ) -> Result<(), WatermarkError> {
        self.state.chats.insert(
            chat_id.to_string(),
            SessionWatermark {
                epoch,
                last_seq,
                pgid,
                process_fingerprint,
            },
        );
        self.write()
    }
}

#[cfg(test)]
#[path = "buffer_test.rs"]
mod buffer_test;
