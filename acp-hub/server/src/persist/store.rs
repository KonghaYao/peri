//! Store：目录初始化（0700/0600）、chat 分片管理、恢复编排（recover）、
//! 磁盘预算记账、degraded 汇聚、归档接口（§7/§9）。
//!
//! 恢复编排（§8.4.1 不变量 1-2，persist 内实现），逐 chat（目录名排序
//! 保证日志确定性）：
//!
//! 1. 清理残留：`updates.snapshot.tmp` 存在 → 删除（rename 未发生，旧日志
//!    完整，§8 崩溃时序 A）；
//! 2. **水位先行加载**（不变量 2 前提）：[`WatermarkStore::load`]；
//! 3. **outbox 重放**（不变量 1）：重建去重索引（§5.4）；
//! 4. **update 日志回放**（§4.4）：尾部截断 + 快照基线选择；
//! 5. **水位对齐**（§6）：与日志尾部核对；
//! 6. 汇总 warnings/degraded/统计 → [`RecoveryResult`]。
//!
//! 与其他层协作（不变量 3-5）：回放记录经 [`Store::replay_outcome`] 交
//! doc-manager 应用；[`Store::recover_signal`] 供 channel 开门门禁；
//! [`Store::status`] 供 Registry `global.status`（§17.2）。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

use uuid::Uuid;

use crate::persist::outbox::{OutboxRecord, OutboxStore, OUTBOX_LOG_TMP_FILE};
use crate::persist::update_log::{UpdateLog, UPDATES_SNAPSHOT_FILE, UPDATES_SNAPSHOT_TMP_FILE};
use crate::persist::watermark::{AlignmentWarning, WatermarkStore};
use crate::persist::{
    DegradedFlag, PersistConfig, PersistStatus, RecoveryResult, RecoveryWarning, StoreError,
    WarningCode,
};

/// chats/ 目录名（§2 目录布局）。
pub const CHATS_DIR: &str = "chats";
/// archive/ 目录名（§2 目录布局）。
pub const ARCHIVE_DIR: &str = "archive";

/// 单 chat 的持久化句柄集合（§10 并发模型：每存储独立锁）。
///
/// - `update_log` / `outbox`：`tokio::sync::Mutex`（异步锁；文件 I/O 为同步
///   小操作，M1 本机可接受）；
/// - `watermark`：`Arc`（内部自锁，append/查询共享）。
pub struct ChatStore {
    chat_id: Uuid,
    dir: PathBuf,
    update_log: Mutex<UpdateLog>,
    outbox: Mutex<OutboxStore>,
    watermark: Arc<WatermarkStore>,
    closed_at: RwLock<Option<DateTime<Utc>>>,
    replay: RwLock<Option<ChatReplay>>,
}

/// chat 恢复产物（不变量 3 数据源：doc-manager 应用回放记录）。
#[derive(Debug, Clone, Default)]
pub struct ChatReplay {
    /// compact 快照基线（§8；`None` = 纯日志回放）。
    pub snapshot: Option<crate::persist::update_log::Snapshot>,
    /// 日志回放记录（按序应用，重复段由聚合器幂等跳过，§4.4）。
    pub records: Vec<crate::persist::update_log::LogRecord>,
    /// 对齐后的水位（补推起点，§8.5）。
    pub watermark: crate::persist::watermark::Watermark,
}

impl ChatStore {
    /// chat id。
    pub fn chat_id(&self) -> Uuid {
        self.chat_id
    }

    /// chat 数据目录。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// update 日志句柄（调用方持锁后调用 [`UpdateLog`] 方法）。
    pub fn update_log(&self) -> &Mutex<UpdateLog> {
        &self.update_log
    }

    /// outbox 句柄（调用方持锁后调用 [`OutboxStore`] 方法）。
    pub fn outbox(&self) -> &Mutex<OutboxStore> {
        &self.outbox
    }

    /// 水位句柄（内部自锁）。
    pub fn watermark(&self) -> &Arc<WatermarkStore> {
        &self.watermark
    }

    /// 记录 chat 关闭时刻（§5.5：control 层在 close 完成时调用；
    /// 清理/归档前置条件）。内存标记【决策：跨重启由 control 层重建】。
    pub fn mark_closed(&self, at: DateTime<Utc>) {
        *self.closed_at.write().expect("closed_at lock poisoned") = Some(at);
    }

    /// 是否已关闭。
    pub fn is_closed(&self) -> bool {
        self.closed_at
            .read()
            .expect("closed_at lock poisoned")
            .is_some()
    }

    /// 关闭时刻。
    pub fn closed_at(&self) -> Option<DateTime<Utc>> {
        *self.closed_at.read().expect("closed_at lock poisoned")
    }

    /// 恢复产物（doc-manager 消费，§7 协作表）。
    pub fn replay_outcome(&self) -> Option<ChatReplay> {
        self.replay.read().expect("replay lock poisoned").clone()
    }

    /// 便捷追加：持 update 锁追加逻辑提交（投影落盘，§4.3）。
    pub async fn append_update(
        &self,
        epoch: u32,
        seq: u64,
        docs: &[(acp_hub_proto::conn::DocId, &[u8])],
    ) -> Result<(), StoreError> {
        self.update_log.lock().await.append(epoch, seq, docs).await
    }

    /// 便捷查询：outbox 去重索引（重发判定，§4.4）。
    pub async fn outbox_get(&self, command_id: Uuid) -> Option<OutboxRecord> {
        self.outbox.lock().await.get(command_id).cloned()
    }
}

/// 磁盘预算报告（§9.2）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetReport {
    /// 已用字节（chats/ + archive/ 全部文件，§9.2 记账范围）。
    pub used: u64,
    /// 预算上限。
    pub limit: u64,
    /// 是否超限。
    pub exceeded: bool,
    /// 淘汰候选（M1 只告警 + 候选提示，不自动删除未满保留期记录，§5.5）。
    pub eviction_candidates: Vec<EvictionCandidate>,
}

/// 淘汰候选（§9.2：最旧已关闭 chat 归档候选 + outbox 最旧终态记录）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvictionCandidate {
    /// 最旧已关闭 chat（归档候选，§9.3 条件检查）。
    ArchiveChat {
        /// chat id。
        chat_id: Uuid,
    },
    /// outbox 最旧终态记录（§5.5 预算淘汰，仍受删除前置条件约束）。
    OutboxTerminal {
        /// 所属 chat。
        chat_id: Uuid,
        /// 命令 id。
        command_id: Uuid,
    },
}

/// 持久化 Store（§7）：目录初始化、chat 分片、恢复编排、预算、degraded
/// 汇聚、归档。
pub struct Store {
    data_dir: PathBuf,
    config: PersistConfig,
    degraded: Arc<DegradedFlag>,
    chats: RwLock<HashMap<Uuid, Arc<ChatStore>>>,
    recovered: AtomicBool,
    recover_signal: Arc<Notify>,
    last_result: RwLock<Option<RecoveryResult>>,
}

impl Store {
    /// 打开数据目录（§9.1）：创建 `data_dir/chats/archive`（0700），校验
    /// 并修复权限；失败 → `StoreError::Io`。不执行恢复（[`Store::recover`]）。
    pub fn open(config: &PersistConfig) -> Result<Self, StoreError> {
        let data_dir = config.data_dir.clone();
        fs::create_dir_all(&data_dir).map_err(|e| StoreError::Io {
            path: data_dir.clone(),
            source: e,
        })?;
        for sub in [CHATS_DIR, ARCHIVE_DIR] {
            let p = data_dir.join(sub);
            fs::create_dir_all(&p).map_err(|e| StoreError::Io {
                path: p.clone(),
                source: e,
            })?;
        }
        set_dir_permissions(&data_dir)?;
        set_dir_permissions(&data_dir.join(CHATS_DIR))?;
        set_dir_permissions(&data_dir.join(ARCHIVE_DIR))?;
        let degraded = Arc::new(DegradedFlag::new());
        Ok(Store {
            data_dir,
            config: config.clone(),
            degraded,
            chats: RwLock::new(HashMap::new()),
            recovered: AtomicBool::new(false),
            recover_signal: Arc::new(Notify::new()),
            last_result: RwLock::new(None),
        })
    }

    /// 恢复编排（§8.4.1 不变量 1-2；完成 = outbox 索引可用 + 水位已对齐）。
    /// 幂等：重复调用返回首次结果。
    pub async fn recover(&self) -> RecoveryResult {
        if let Some(r) = self
            .last_result
            .read()
            .expect("last_result lock poisoned")
            .clone()
        {
            return r;
        }
        let result = self.recover_once();
        *self.last_result.write().expect("last_result lock poisoned") = Some(result.clone());
        self.recovered.store(true, Ordering::SeqCst);
        self.recover_signal.notify_waiters();
        info!(
            data_dir = %self.data_dir.display(),
            degraded = result.degraded, warnings = result.warnings.len(),
            truncated_bytes = result.truncated_total_bytes,
            "persist recover complete"
        );
        result
    }

    /// 恢复编排实现（同步；逐 chat 目录名排序保证日志确定性，§7）。
    fn recover_once(&self) -> RecoveryResult {
        let mut result = RecoveryResult::default();
        let chats_dir = self.data_dir.join(CHATS_DIR);
        let mut dirs: Vec<PathBuf> = match fs::read_dir(&chats_dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .map(|e| e.path())
                .collect(),
            Err(e) => {
                warn!(path = %chats_dir.display(), error = %e, "chats dir unreadable");
                self.degraded.set(format!("chats dir unreadable: {e}"));
                result.degraded = true;
                return result;
            }
        };
        dirs.sort();
        for dir in dirs {
            let Some(name) = dir.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Ok(chat_id) = Uuid::parse_str(name) else {
                // 非 uuid 目录：不参与恢复（防御；不告警以免噪音）。
                continue;
            };
            let chat = match self.open_chat(&dir, chat_id, &mut result) {
                Ok(s) => s,
                Err(e) => {
                    warn!(
                        chat_id = %chat_id, path = %dir.display(), error = %e,
                        "chat recover failed; store degraded"
                    );
                    self.degraded
                        .set(format!("chat {chat_id} recover failed: {e}"));
                    result.degraded = true;
                    continue;
                }
            };
            self.chats
                .write()
                .expect("chats lock poisoned")
                .insert(chat_id, chat);
        }
        result
    }

    /// 打开并恢复单个 chat（§7 步骤 1-5）。
    fn open_chat(
        &self,
        dir: &Path,
        chat_id: Uuid,
        result: &mut RecoveryResult,
    ) -> Result<Arc<ChatStore>, StoreError> {
        set_dir_permissions(dir)?;
        // 1. 清理残留：updates.snapshot.tmp（rename 未发生 → 旧日志完整，
        //    §8 崩溃时序 A）与 outbox.log.tmp（物理压缩崩溃残留，§5.5）。
        let tmp_snapshot = dir.join(UPDATES_SNAPSHOT_TMP_FILE);
        if tmp_snapshot.exists() {
            fs::remove_file(&tmp_snapshot).map_err(|e| StoreError::Io {
                path: tmp_snapshot.clone(),
                source: e,
            })?;
            debug!(chat_id = %chat_id, "removed stale snapshot tmp");
        }
        let tmp_outbox = dir.join(OUTBOX_LOG_TMP_FILE);
        if tmp_outbox.exists() {
            fs::remove_file(&tmp_outbox).map_err(|e| StoreError::Io {
                path: tmp_outbox.clone(),
                source: e,
            })?;
            debug!(chat_id = %chat_id, "removed stale outbox tmp");
        }
        // 2. 水位先行加载（不变量 2 前提）。
        let watermark = Arc::new(WatermarkStore::open(
            dir,
            self.config.fsync_mode,
            self.degraded.clone(),
        ));
        let wm = watermark.load()?;
        if wm.is_none() && watermark.path().exists() {
            // 文件存在但加载为 None = 损坏（CRC/解析失败）→ degraded 已由
            // load 置位 + 告警；这里补充 RecoveryWarning 聚合。
            result.warnings.push(RecoveryWarning {
                code: WarningCode::WatermarkCorrupt,
                path: watermark.path().to_path_buf(),
                message: "watermark file corrupt; treated as absent".into(),
            });
            result.degraded = true;
        }
        // 3. outbox 重放（不变量 1：重建去重索引，§5.4）。
        let mut outbox = OutboxStore::open(
            dir,
            self.config.fsync_mode,
            self.config.outbox_retention,
            self.degraded.clone(),
        )?;
        let outbox_replay = outbox.replay_from_disk()?;
        if let Some(t) = &outbox_replay.truncated {
            result.warnings.push(RecoveryWarning {
                code: WarningCode::TailTruncated,
                path: outbox.path().to_path_buf(),
                message: format!("outbox log tail truncated: {}", t.reason),
            });
            result.truncated_total_bytes += t.bytes_kept;
        }
        result
            .corrupt_artifacts
            .extend(outbox_replay.corrupt_artifacts);
        if outbox_replay.degraded {
            result.degraded = true;
        }
        // 4. update 日志回放（§4.4）：快照基线选择 + 尾部截断。
        let mut update_log = UpdateLog::open(
            dir,
            chat_id,
            watermark.clone(),
            self.config.fsync_mode,
            self.config.compact_threshold_bytes,
            self.config.compact_interval,
            self.degraded.clone(),
        )?;
        let snapshot = match update_log.load_snapshot() {
            Ok(s) => s,
            Err(e) => {
                // 快照读取 I/O 失败 → 纯日志回放 + degraded。
                warn!(chat_id = %chat_id, error = %e, "snapshot load failed");
                self.degraded.set(format!("snapshot load failed: {e}"));
                result.degraded = true;
                None
            }
        };
        if let Some(reason) = update_log.snapshot_invalid_reason() {
            // 快照失效（CRC/解析失败/version 不符）→ 移 corrupt/ + degraded
            // 已由 load_snapshot 处理；这里补充聚合告警（§8）。
            result.warnings.push(RecoveryWarning {
                code: WarningCode::SnapshotInvalid,
                path: dir.join(UPDATES_SNAPSHOT_FILE),
                message: format!("snapshot invalid, fell back to pure log replay: {reason}"),
            });
        }
        if let Some(s) = &snapshot {
            // 日志尾部与快照点核对：日志完整且全部记录 ≤ 快照点 → 截断日志
            // （§8 崩溃时序 B 恢复；幂等跳过重复段由聚合器兜底）。
            //
            // 必须且仅当尾部探测**完整**（无 CRC/结构损坏）时执行：probe 停
            // 在首个损坏点会低估 last_seq，提前截断会把损坏点之后的完好记录
            // 静默清空——绕过 §8.4 的告警/degraded/诊断保留契约。探测到损坏
            // 时交给 replay 走完整信号路径。
            if !update_log.tail_probe_corrupted() && update_log.last_seq() <= s.last_applied_seq {
                match update_log.truncate_after_snapshot() {
                    Ok(()) => {}
                    Err(e) => {
                        warn!(chat_id = %chat_id, error = %e, "log truncate after snapshot failed")
                    }
                }
            }
        }
        // 4. update 日志回放（§4.4）：尾部截断（损坏 → 截断 + 告警 +
        //    degraded + corrupt 段保留）。
        let replay = update_log.replay()?;
        if let Some(t) = &replay.truncated {
            result.warnings.push(RecoveryWarning {
                code: WarningCode::TailTruncated,
                path: update_log.path().to_path_buf(),
                message: format!("update log tail truncated: {}", t.reason),
            });
            result.truncated_total_bytes += t.bytes_kept;
        }
        result
            .corrupt_artifacts
            .extend(update_log.replay_corrupt_artifacts().iter().cloned());
        if replay.degraded {
            result.degraded = true;
        }
        result.warnings.extend(replay.warnings.iter().cloned());
        // 5. 水位对齐（§6/§8.4.1 不变量 2）。
        // 日志尾部 = 回放最后一条；日志为空且快照存在时以快照点为等效尾部
        // （§8 崩溃时序 C：日志已被截断，快照携带 (last_epoch, last_applied_seq)
        // 边界；否则水位损坏/缺失时对齐到 (0,0)，补推起点与真实代际脱节，
        // epoch 不匹配会被 instance 侧拒绝——§4.5.1）。
        let log_tail = if replay.records.is_empty() {
            snapshot
                .as_ref()
                .map(|s| (s.last_epoch, s.last_applied_seq))
        } else {
            let last = replay.records.last().expect("non-empty");
            Some((last.epoch, last.seq))
        };
        let (aligned, align_warning) = watermark.align(wm, log_tail);
        if let Some(w) = align_warning {
            let (code, message) = match w {
                AlignmentWarning::SeqMismatch {
                    watermark_seq,
                    log_seq,
                } => (
                    WarningCode::SeqMismatch,
                    format!("watermark seq {watermark_seq} vs log seq {log_seq}; taking min"),
                ),
                AlignmentWarning::EpochMismatch {
                    watermark_epoch,
                    log_epoch,
                } => (
                    WarningCode::EpochMismatch,
                    format!("watermark epoch {watermark_epoch} vs log epoch {log_epoch}; watermark authoritative"),
                ),
            };
            result.warnings.push(RecoveryWarning {
                code,
                path: watermark.path().to_path_buf(),
                message,
            });
        }
        // 汇总：恢复不变量失败 → degraded（§17.2）；快照失效告警。
        if update_log.degraded() || watermark.degraded_is_set() || outbox.degraded_is_set() {
            result.degraded = true;
        }
        let chat_replay = ChatReplay {
            snapshot,
            records: replay.records,
            watermark: aligned,
        };
        let chat = Arc::new(ChatStore {
            chat_id,
            dir: dir.to_path_buf(),
            update_log: Mutex::new(update_log),
            outbox: Mutex::new(outbox),
            watermark,
            closed_at: RwLock::new(None),
            replay: RwLock::new(Some(chat_replay)),
        });
        Ok(chat)
    }

    /// 获取 chat 句柄（已恢复/已创建）。
    pub fn chat(&self, chat_id: Uuid) -> Option<Arc<ChatStore>> {
        self.chats
            .read()
            .expect("chats lock poisoned")
            .get(&chat_id)
            .cloned()
    }

    /// 全部 chat 快照（`(chat_id, 句柄)`；create 全局去重索引重建用，
    /// §4.4：跨 chat 按 commandId 查）。
    pub fn chats_snapshot(&self) -> Vec<(Uuid, Arc<ChatStore>)> {
        self.chats
            .read()
            .expect("chats lock poisoned")
            .iter()
            .map(|(id, s)| (*id, s.clone()))
            .collect()
    }

    /// 新建 chat（§4.4：server 生成 uuid；目录即持久化创建点）。
    pub fn create_chat(&self, chat_id: Uuid) -> Result<Arc<ChatStore>, StoreError> {
        let dir = self.data_dir.join(CHATS_DIR).join(chat_id.to_string());
        fs::create_dir_all(&dir).map_err(|e| StoreError::Io {
            path: dir.clone(),
            source: e,
        })?;
        set_dir_permissions(&dir)?;
        let mut result = RecoveryResult::default();
        let chat = self.open_chat(&dir, chat_id, &mut result)?;
        if result.degraded {
            warn!(
                chat_id = %chat_id, warnings = ?result.warnings,
                "new chat opened degraded"
            );
        }
        self.chats
            .write()
            .expect("chats lock poisoned")
            .insert(chat_id, chat.clone());
        info!(chat_id = %chat_id, "chat persist store created");
        Ok(chat)
    }

    /// 移除 chat 目录（close 后的最终清理；归档走
    /// [`Store::archive_chat`]）。调用方保证无在途写入。
    pub fn remove_chat(&self, chat_id: Uuid) -> Result<(), StoreError> {
        let dir = self.data_dir.join(CHATS_DIR).join(chat_id.to_string());
        if dir.exists() {
            fs::remove_dir_all(&dir).map_err(|e| StoreError::Io {
                path: dir.clone(),
                source: e,
            })?;
        }
        self.chats
            .write()
            .expect("chats lock poisoned")
            .remove(&chat_id);
        info!(chat_id = %chat_id, "chat persist store removed");
        Ok(())
    }

    /// 归档 chat（§9.3）：条件检查——chat 已关闭（`mark_closed`）+
    /// outbox 全终态 + `closed_at + archive_retention` 届满；满足则移动目录
    /// 至 `archive/<chat_id>` 并记录清单。M1 简化：自动触发由启动巡检 +
    /// 预算巡检调用（§9.3）；归档内容（压缩/导出）后置开放问题 3。
    ///
    /// 返回 `Ok(true)` = 已归档；`Ok(false)` = 条件不满足（原因经
    /// `tracing::debug!` 记录，不告警——未届满属正常时序）。
    pub fn archive_chat(&self, chat_id: Uuid, now: DateTime<Utc>) -> Result<bool, StoreError> {
        let chat = match self.chat(chat_id) {
            Some(s) => s,
            None => return Err(StoreError::ChatNotFound { chat_id }),
        };
        let closed_at = match chat.closed_at() {
            Some(t) => t,
            None => {
                debug!(chat_id = %chat_id, "archive skipped: chat not closed");
                return Ok(false);
            }
        };
        if closed_at + chrono::Duration::from_std(self.config.archive_retention).unwrap_or_default()
            > now
        {
            debug!(chat_id = %chat_id, "archive skipped: retention not elapsed");
            return Ok(false);
        }
        let all_terminal = chat
            .outbox
            .try_lock()
            .map(|o| o.records().all(|r| r.status.is_terminal()))
            .unwrap_or(false);
        if !all_terminal {
            debug!(chat_id = %chat_id, "archive skipped: outbox not all terminal");
            return Ok(false);
        }
        let src = self.data_dir.join(CHATS_DIR).join(chat_id.to_string());
        let dst = self.data_dir.join(ARCHIVE_DIR).join(chat_id.to_string());
        fs::create_dir_all(dst.parent().expect("archive dir")).map_err(|e| StoreError::Io {
            path: dst.clone(),
            source: e,
        })?;
        fs::rename(&src, &dst).map_err(|e| StoreError::Io {
            path: src.clone(),
            source: e,
        })?;
        self.chats
            .write()
            .expect("chats lock poisoned")
            .remove(&chat_id);
        info!(
            chat_id = %chat_id, archive_path = %dst.display(),
            "chat archived"
        );
        Ok(true)
    }

    /// 运行期状态（§7 不变量 5 数据源；Registry `global.status` 消费）。
    pub fn status(&self) -> PersistStatus {
        PersistStatus {
            degraded: self.degraded.is_set(),
            reason: self.degraded.reason(),
            disk_used: self.disk_used(),
            disk_limit: self.config.disk_budget,
        }
    }

    /// 磁盘占用（§9.2 记账范围：chats/ + archive/ 全部文件，含 corrupt）。
    pub fn disk_used(&self) -> u64 {
        let mut total = 0u64;
        for sub in [CHATS_DIR, ARCHIVE_DIR] {
            total += dir_size(&self.data_dir.join(sub));
        }
        total
    }

    /// 预算检查（§9.2 检查点：append / compact / cleanup 之后调用）：
    /// 超限 → 告警（绝不静默）+ 淘汰候选（最旧已关闭 chat + 最旧终态
    /// outbox 记录）；持续超限且无可淘汰 → degraded（落盘失败语义同源）。
    ///
    /// M1 简化（§5.5/§9.2）：只告警 + 候选提示，不自动删除未满保留期记录；
    /// 自动触发由启动巡检 + 预算巡检调用。
    pub fn enforce_budget(&self) -> BudgetReport {
        let used = self.disk_used();
        let limit = self.config.disk_budget;
        let exceeded = used > limit;
        let candidates = if exceeded {
            self.budget_candidates()
        } else {
            Vec::new()
        };
        if exceeded {
            warn!(
                event = "disk_budget.exceeded",
                used,
                limit,
                candidates = candidates.len(),
                "disk budget exceeded"
            );
            if candidates.is_empty() {
                self.degraded.set(format!(
                    "disk budget exceeded ({used}B > {limit}B) with no eviction candidates"
                ));
            }
        }
        BudgetReport {
            used,
            limit,
            exceeded,
            eviction_candidates: candidates,
        }
    }

    /// 淘汰候选（§9.2）：最旧已关闭 chat（归档候选）+ 各 chat 最旧
    /// 终态 outbox 记录。不修改任何数据。
    fn budget_candidates(&self) -> Vec<EvictionCandidate> {
        let mut candidates = Vec::new();
        let chats = self.chats.read().expect("chats lock poisoned");
        // 最旧已关闭 chat（归档候选，§9.3 条件仍要满足）。
        let mut oldest_closed: Option<(Uuid, DateTime<Utc>)> = None;
        for chat in chats.values() {
            if let Some(closed) = chat.closed_at() {
                if oldest_closed
                    .as_ref()
                    .map(|(_, t)| closed < *t)
                    .unwrap_or(true)
                {
                    oldest_closed = Some((chat.chat_id(), closed));
                }
            }
            // 最旧终态记录（§5.5 预算淘汰：不受 7 天约束，仍受前置条件约束）。
            if let Ok(outbox) = chat.outbox.try_lock() {
                let mut oldest: Option<(&OutboxRecord, DateTime<Utc>)> = None;
                for r in outbox.records() {
                    if r.status.is_terminal()
                        && oldest
                            .as_ref()
                            .map(|(_, t)| r.updated_at < *t)
                            .unwrap_or(true)
                    {
                        oldest = Some((r, r.updated_at));
                    }
                }
                if let Some((r, _)) = oldest {
                    candidates.push(EvictionCandidate::OutboxTerminal {
                        chat_id: chat.chat_id(),
                        command_id: r.command_id,
                    });
                }
            }
        }
        if let Some((chat_id, _)) = oldest_closed {
            candidates.push(EvictionCandidate::ArchiveChat { chat_id });
        }
        candidates
    }

    /// 恢复完成信号（§7 不变量 4 门禁前置条件；channel 在
    /// [`Store::is_recovered`] 为 true 前不得接受任何 Action）。
    pub fn recover_signal(&self) -> Arc<Notify> {
        self.recover_signal.clone()
    }

    /// 恢复是否已完成（开门门禁检查）。
    pub fn is_recovered(&self) -> bool {
        self.recovered.load(Ordering::SeqCst)
    }

    /// 最近一次恢复结果（幂等 recover 的缓存；未恢复 = `None`）。
    pub fn last_recovery(&self) -> Option<RecoveryResult> {
        self.last_result
            .read()
            .expect("last_result lock poisoned")
            .clone()
    }

    /// chat 回放产物（doc-manager 应用，不变量 3）。
    pub fn replay_outcome(&self, chat_id: Uuid) -> Option<ChatReplay> {
        self.chat(chat_id)?.replay_outcome()
    }

    /// 数据目录。
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// 配置（只读副本）。
    pub fn config(&self) -> &PersistConfig {
        &self.config
    }
}

impl ChatStore {
    /// 便捷：当前是否 degraded（任一路径）。
    pub fn degraded(&self) -> bool {
        self.watermark.degraded_is_set()
            || self
                .update_log
                .try_lock()
                .map(|l| l.degraded())
                .unwrap_or(false)
            || self
                .outbox
                .try_lock()
                .map(|o| o.degraded_is_set())
                .unwrap_or(false)
    }
}

/// 目录权限修复（§9.1：目录 0700）。
fn set_dir_permissions(dir: &Path) -> Result<(), StoreError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).map_err(|e| StoreError::Io {
            path: dir.to_path_buf(),
            source: e,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// 递归目录大小（§9.2 记账；含 corrupt 段——约束诊断膨胀）。
fn dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += dir_size(&path);
            } else if let Ok(m) = entry.metadata() {
                total += m.len();
            }
        }
    }
    total
}
