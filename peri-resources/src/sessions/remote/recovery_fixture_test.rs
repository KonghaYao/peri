//! 连接重建/关闭测试共用的**可控假远端**夹具（离线，不联网）。
//!
//! 这里只有装配与观察点，没有断言。假传输与生产传输在同一个 trait 上（[`RemoteTransport`]），
//! 重建走同一条 `ConnectionFactory` 路径，因此两个测试模块
//! （`connection_recovery_test.rs` 重建、`connection_close_test.rs` 关闭）断言的是生产判定，
//! 不是另一份实现。
//!
//! 观察点分三类：服务端事实（身份、账本、已提交的批、收到的请求）、故障档位（挂起下一次
//! 读取/批、关闭失败或挂起、把某一代记为失效）、以及每次真实关闭尝试的连接号记录。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use peri_acp_types::session_resources::SessionResourceResult;
use peri_acp_types::store::PersistedPayload;
use peri_acp_types::thread::ThreadId;
use turso_serverless::{Error as SdkError, Value};

use super::connection::RemoteTransport;
use super::generation::{ConnectionFactory, ConnectionGate};
use super::ledger::{self, OperationId, OperationIdentity};
use super::mutation::{RemoteStore, StoreAccess};
use super::schema::{self, StoreId, REMOTE_SCHEMA_VERSION, STORE_CONTRACT};
use super::session_data::RemoteSessionData;
use super::session_sql;
use super::sql::{text_at, StatementSpec};
use crate::sessions::data::SessionDataPort;

pub(super) const ABANDON_AFTER: Duration = Duration::from_millis(50);

// ─── 假远端事实 ───────────────────────────────────────────────────────────────

/// 挂起档位：读，或批（`committed` = 批已在远端提交、响应在回程丢失）。
#[derive(Clone, Copy)]
pub(super) enum Hang {
    Read,
    Batch { committed: bool },
}

/// 跨连接共享的「服务端」事实：脚本、账本与请求日志。
#[derive(Default)]
pub(super) struct FakeBackend {
    /// store 身份（重建时重核实用）。
    store_id: Mutex<Option<String>>,
    /// 会话事实行在不在（决定「正确回答」是 `NotFound` 还是那个会话的分类）。
    pub(super) session_row: AtomicBool,
    /// 下一次命中档位的调用永远挂起。
    hang: Mutex<Option<Hang>>,
    /// 已经建立过多少条连接。
    pub(super) connections: AtomicU64,
    /// 假账本：operation id → (state, receipt, digest)。
    ledger: Mutex<HashMap<String, FakeLedgerRow>>,
    /// 服务端真正提交了的托管批（含效果语句数）。
    executed: Mutex<Vec<Executed>>,
    /// 服务端收到的请求（连接号 + SQL + 参数）。
    issued: Mutex<Vec<Issued>>,
    /// 关闭是否失败（「关闭失败后不复活」的反例开关）。
    pub(super) fail_close: AtomicBool,
    /// 下一次传输读取顺手把这一代记为失效（「检查与取守卫之间又落下失效事实」的可控复现）。
    retire_next_generation: Mutex<Option<u64>>,
    /// 本夹具的代际门禁（装配时挂上：失效档位必须走真实门禁，不另造一份判定）。
    gate: Mutex<Option<Arc<ConnectionGate>>>,
    /// 在途关闭是否挂起（「关闭被取消」的反例开关）：挂起时只有测试放行才返回。
    pub(super) hold_close: AtomicBool,
    /// 放行挂起的在途关闭。
    pub(super) close_release: tokio::sync::Notify,
    /// 每一次真实关闭尝试的连接号（按调用顺序）：「重试关的是同一条连接」的证据。
    close_attempts: Mutex<Vec<u64>>,
    /// 真实关闭**成功**的次数。
    close_successes: AtomicU64,
}

/// 假账本的一行：状态、收据与输入摘要。
#[derive(Clone)]
struct FakeLedgerRow {
    state: String,
    receipt: Option<String>,
    digest: String,
}

struct Executed {
    effects: usize,
}

struct Issued {
    connection: u64,
    sql: &'static str,
    params: Vec<Value>,
}

impl FakeBackend {
    fn new(store_id: &str) -> Self {
        Self {
            store_id: Mutex::new(Some(store_id.to_owned())),
            ..Self::default()
        }
    }

    /// 挂上本夹具的代际门禁（装配时一次）。
    fn attach_gate(&self, gate: &Arc<ConnectionGate>) {
        *self.gate.lock().unwrap() = Some(Arc::clone(gate));
    }

    /// 让**下一次**传输读取顺手把指定代际记为失效。
    pub(super) fn retire_generation_on_next_read(&self, generation: u64) {
        *self.retire_next_generation.lock().unwrap() = Some(generation);
    }

    /// 取出并落下这次失效（没有档位时什么也不做）。
    fn retire_queued_generation(&self) {
        let Some(generation) = self.retire_next_generation.lock().unwrap().take() else {
            return;
        };
        let gate = {
            let attached = self.gate.lock().unwrap();
            Arc::clone(attached.as_ref().expect("fixture gate is attached"))
        };
        // 丢弃一次守卫＝落下一次失效事实（与在途调用被放弃同一路径）。
        drop(gate.lease(generation));
    }

    pub(super) fn hang_next(&self, hang: Hang) {
        *self.hang.lock().unwrap() = Some(hang);
    }

    fn take_read_hang(&self) -> Option<Hang> {
        self.take_hang(|hang| matches!(hang, Hang::Read))
    }

    fn take_batch_hang(&self) -> Option<bool> {
        self.take_hang(|hang| matches!(hang, Hang::Batch { .. }))
            .map(|hang| matches!(hang, Hang::Batch { committed: true }))
    }

    fn take_hang(&self, wanted: impl Fn(&Hang) -> bool) -> Option<Hang> {
        let mut hang = self.hang.lock().unwrap();
        if hang.as_ref().is_some_and(wanted) {
            hang.take()
        } else {
            None
        }
    }

    pub(super) fn connections(&self) -> u64 {
        self.connections.load(Ordering::SeqCst)
    }

    /// 按调用顺序记录的真实关闭尝试（每条是被关的连接号）。
    pub(super) fn close_attempts(&self) -> Vec<u64> {
        self.close_attempts.lock().unwrap().clone()
    }

    pub(super) fn close_successes(&self) -> u64 {
        self.close_successes.load(Ordering::SeqCst)
    }

    pub(super) fn issued(&self) -> Vec<(u64, &'static str, Vec<Value>)> {
        self.issued
            .lock()
            .unwrap()
            .iter()
            .map(|issue| (issue.connection, issue.sql, issue.params.clone()))
            .collect()
    }

    /// 提交一批：账本行按真实语句的参数落盘，效果语句计数。
    fn execute(&self, statements: &[StatementSpec]) {
        let sql = ledger_sql();
        let mut effects = 0;
        for spec in statements {
            if spec.sql == sql.qualify {
                let id = text_at(&spec.params, 0).unwrap_or_default().to_owned();
                let digest = text_at(&spec.params, 2).unwrap_or_default().to_owned();
                let receipt = text_at(&spec.params, 3).map(str::to_owned);
                self.ledger.lock().unwrap().insert(
                    id.clone(),
                    FakeLedgerRow {
                        state: "applied".to_owned(),
                        receipt,
                        digest,
                    },
                );
            } else if spec.sql == sql.closure {
                let id = text_at(&spec.params, 0).unwrap_or_default().to_owned();
                let digest = text_at(&spec.params, 2).unwrap_or_default().to_owned();
                self.ledger.lock().unwrap().insert(
                    id.clone(),
                    FakeLedgerRow {
                        state: "closed".to_owned(),
                        receipt: None,
                        digest,
                    },
                );
            } else {
                effects += 1;
            }
        }
        self.executed.lock().unwrap().push(Executed { effects });
    }

    /// 已提交的**业务效果批**数（账本行不算效果）：数据至多一份就是它 ≤ 1。
    pub(super) fn executed_effect_batches(&self) -> usize {
        self.executed
            .lock()
            .unwrap()
            .iter()
            .filter(|batch| batch.effects > 0)
            .count()
    }

    /// 一条只读语句的假回答。
    fn answer_read(&self, spec: &StatementSpec) -> Vec<Vec<Value>> {
        let identity = schema::identity_read_plan();
        if spec.sql == identity[0].sql {
            return vec![vec![Value::Text("peri_store_meta".to_owned())]];
        }
        if spec.sql == identity[1].sql {
            let store_id = self.store_id.lock().unwrap().clone().unwrap_or_default();
            return vec![vec![
                Value::Integer(REMOTE_SCHEMA_VERSION),
                Value::Text(store_id),
                Value::Text(STORE_CONTRACT.to_owned()),
            ]];
        }
        if spec.sql == ledger_sql().resolve {
            let id = text_at(&spec.params, 0).unwrap_or_default().to_owned();
            let row = self.ledger.lock().unwrap().get(&id).cloned();
            return match row {
                Some(row) => vec![vec![
                    Value::Text(row.state),
                    row.receipt.map_or(Value::Null, Value::Text),
                    Value::Text(row.digest),
                ]],
                None => Vec::new(),
            };
        }
        if spec.sql == session_sql::select_meta_statement("probe").sql {
            return if self.session_row.load(Ordering::SeqCst) {
                vec![vec![Value::Null; session_sql::META_AGENT_STATUS + 1]]
            } else {
                Vec::new()
            };
        }
        if spec.sql == session_sql::select_session_statement("probe").sql {
            return if self.session_row.load(Ordering::SeqCst) {
                vec![vec![Value::Null; session_sql::FACT_COLUMN_TOTAL]]
            } else {
                Vec::new()
            };
        }
        // 其余读取（root 上溯等）一律「没有这一行」：调用方按事实不完整处理。
        Vec::new()
    }
}

/// 一次在途调用被丢弃之后，那一代连接作废（复现 SDK 的毒化），后续请求在连接层失败。
struct FakeConnection {
    backend: Arc<FakeBackend>,
    number: u64,
    dead: AtomicBool,
}

impl FakeConnection {
    /// 与 SDK 观察到的形状一致：被丢弃的在途请求之后，这条连接上的请求在连接层失败。
    fn reject_if_dead(&self) -> turso_serverless::Result<()> {
        if self.dead.load(Ordering::Acquire) {
            return Err(SdkError::Http(
                "fake connection: stream is unusable after an abandoned request".to_owned(),
            ));
        }
        Ok(())
    }

    fn record(&self, spec: &StatementSpec) {
        self.backend.issued.lock().unwrap().push(Issued {
            connection: self.number,
            sql: spec.sql,
            params: spec.params.clone(),
        });
    }

    /// 命中挂起档位就永远挂起：只有调用方丢弃这个 future 才能脱身，丢弃即本连接作废。
    async fn hang_forever(&self) {
        let _guard = DeadOnDrop { dead: &self.dead };
        std::future::pending::<()>().await;
    }
}

/// 被丢弃时把连接记为不可用。
struct DeadOnDrop<'a> {
    dead: &'a AtomicBool,
}

impl Drop for DeadOnDrop<'_> {
    fn drop(&mut self) {
        self.dead.store(true, Ordering::Release);
    }
}

#[async_trait]
impl RemoteTransport for FakeConnection {
    async fn sql_values(&self, spec: &StatementSpec) -> turso_serverless::Result<Vec<Vec<Value>>> {
        self.reject_if_dead()?;
        self.backend.retire_queued_generation();
        self.record(spec);
        if self.backend.take_read_hang().is_some() {
            self.hang_forever().await;
        }
        Ok(self.backend.answer_read(spec))
    }

    async fn managed_batch(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<u64>> {
        self.reject_if_dead()?;
        for spec in &statements {
            self.record(spec);
        }
        if let Some(committed) = self.backend.take_batch_hang() {
            if committed {
                // 批真的提交了，响应在回程丢失：账本行是远端事实，恢复必须读回去。
                self.backend.execute(&statements);
            }
            self.hang_forever().await;
        }
        self.backend.execute(&statements);
        Ok(vec![1; statements.len()])
    }

    async fn consistent_read(
        &self,
        statements: Vec<StatementSpec>,
    ) -> turso_serverless::Result<Vec<Vec<Vec<Value>>>> {
        self.reject_if_dead()?;
        self.backend.retire_queued_generation();
        let mut sets = Vec::with_capacity(statements.len());
        for spec in &statements {
            self.record(spec);
            sets.push(self.backend.answer_read(spec));
        }
        Ok(sets)
    }

    fn is_autocommit(&self) -> turso_serverless::Result<bool> {
        Ok(true)
    }

    async fn close(&self) -> turso_serverless::Result<()> {
        self.backend
            .close_attempts
            .lock()
            .unwrap()
            .push(self.number);
        // 在途关闭可以被挂起：调用方丢弃 future 时，唯一句柄必须还在 adapter 的关闭句柄里。
        let release = self.backend.close_release.notified();
        if self.backend.hold_close.load(Ordering::SeqCst) {
            release.await;
        }
        if self.backend.fail_close.load(Ordering::SeqCst) {
            return Err(SdkError::Http("fake close failed".to_owned()));
        }
        self.backend.close_successes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

/// 假工厂：每调用一次建立一条新的假连接（与生产工厂同一条重建路径）。
struct FakeFactory {
    backend: Arc<FakeBackend>,
    gate: Arc<ConnectionGate>,
    access: StoreAccess,
}

#[async_trait]
impl ConnectionFactory for FakeFactory {
    async fn connect(&self) -> SessionResourceResult<RemoteStore> {
        Ok(fake_store(&self.backend, &self.gate, self.access))
    }
}

fn fake_store(
    backend: &Arc<FakeBackend>,
    gate: &Arc<ConnectionGate>,
    access: StoreAccess,
) -> RemoteStore {
    let number = backend.connections.fetch_add(1, Ordering::SeqCst) + 1;
    RemoteStore::new(
        Arc::new(FakeConnection {
            backend: Arc::clone(backend),
            number,
            dead: AtomicBool::new(false),
        }),
        access,
        gate.mint(),
        Arc::clone(gate),
    )
}

/// 账本三类语句的真实 SQL 文本（不在测试里另抄一份）。
pub(super) struct LedgerSql {
    pub(super) qualify: &'static str,
    pub(super) closure: &'static str,
    pub(super) resolve: &'static str,
}

pub(super) fn ledger_sql() -> LedgerSql {
    let identity = OperationIdentity::new(OperationId::from_record("probe"), "probe", &[]);
    LedgerSql {
        qualify: ledger::qualify_statement(&identity, "0").sql,
        closure: ledger::closure_statement(&identity, "0").sql,
        resolve: ledger::resolve_statement(&identity.operation_id).sql,
    }
}

// ─── 装配 ─────────────────────────────────────────────────────────────────────

pub(super) struct Harness {
    /// 被测 adapter（`Arc`：门面装配测试与业务侧指向同一份事实）。
    pub(super) adapter: Arc<RemoteSessionData>,
    pub(super) backend: Arc<FakeBackend>,
    pub(super) gate: Arc<ConnectionGate>,
}

impl Harness {
    pub(super) async fn open(access: StoreAccess) -> Self {
        let store_id = StoreId::mint();
        let store_key = store_id.as_str().to_owned();
        let gate = Arc::new(ConnectionGate::default());
        let backend = Arc::new(FakeBackend::new(&store_key));
        backend.attach_gate(&gate);
        let connection = fake_store(&backend, &gate, access);
        let factory: Arc<dyn ConnectionFactory> = Arc::new(FakeFactory {
            backend: Arc::clone(&backend),
            gate: Arc::clone(&gate),
            access,
        });
        let adapter = RemoteSessionData::with_connection_for_test(
            store_id,
            connection,
            factory,
            Arc::clone(&gate),
        );
        Self {
            adapter: Arc::new(adapter),
            backend,
            gate,
        }
    }
}

/// 丢掉一次在途读取：请求已经发出、答案没有回来。
pub(super) async fn abandon_read(harness: &Harness, id: &ThreadId) {
    harness.backend.hang_next(Hang::Read);
    let abandoned = tokio::time::timeout(ABANDON_AFTER, harness.adapter.load_binding(id)).await;
    assert!(abandoned.is_err(), "the read was expected to be abandoned");
}

/// 丢掉一次在途写入：批已经发出（`committed` 决定远端有没有提交），响应没有回来。
pub(super) async fn abandon_write(harness: &Harness, id: &ThreadId, committed: bool) {
    harness.backend.hang_next(Hang::Batch { committed });
    let payloads = vec![PersistedPayload::Message(
        peri_acp_types::messages::BaseMessage::human("abandoned"),
    )];
    let abandoned =
        tokio::time::timeout(ABANDON_AFTER, harness.adapter.append_history(id, &payloads)).await;
    assert!(
        abandoned.is_err(),
        "the write was expected to be abandoned in flight"
    );
}
