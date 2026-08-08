//! Doc 创建与结构补齐（§5.6 schema_version/projection_version 分离；§8.4.1
//! 「Doc 补齐」）。

use yrs::{Map, Transact, WriteTxn};

use acp_hub_proto::version::{
    CHAT_DOC_SCHEMA_VERSION, REGISTRY_DOC_SCHEMA_VERSION, SESSION_DOC_SCHEMA_VERSION,
};

use crate::state::doc_pair::{DocPair, StreamState};
use crate::state::view_store::TransactionCtx;

/// 根 Map 键名（yrs 根对象命名，§5.3「根对象用 Y.Map」）。
pub const ROOT: &str = "root";

/// Doc 种类（ensure_schema 的分派键）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// Chat Doc（`chat:{session_id}`，§5.3）。
    Chat,
    /// Session Doc（`session:{session_id}`，§5.4）。
    Session,
    /// Registry Doc（`hub:registry`，§5.5）。
    Registry,
}

/// Factory 错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FactoryError {
    /// 快照 schema_version 大于当前实现版本（启动恢复不变量失败路径，上报
    /// §12.5 degraded）。
    #[error("future schema version: found {found}, expected {expected}")]
    FutureSchema { found: u32, expected: u32 },
}

/// Doc 创建与结构补齐（§5.6 schema_version/projection_version 分离；§8.4.1
/// 「Doc 补齐」）。
///
/// 不假设旧快照完整：重放后以 schema_version 判空幂等补结构，缺失键补空结构、
/// 不覆盖已有数据；旧客户端忽略未知字段仍安全（服务端是唯一写入者）。
pub struct Factory {
    /// 各 Doc 的当前 schema_version（proto 版本常量，§5.3/5.4/5.5）。
    chat_schema: u32,
    session_schema: u32,
    registry_schema: u32,
}

impl Factory {
    /// 以 proto 版本常量构造（§5.3/5.4/5.5 版本事实源）。
    pub fn new() -> Self {
        Factory {
            chat_schema: CHAT_DOC_SCHEMA_VERSION,
            session_schema: SESSION_DOC_SCHEMA_VERSION,
            registry_schema: REGISTRY_DOC_SCHEMA_VERSION,
        }
    }

    /// 创建空 Chat/Session Doc（M1 简化：直接建全结构，§6【决策】）。
    pub fn create_chat_doc(&self) -> DocPair {
        let mut chat = yrs::Doc::new();
        let mut session = yrs::Doc::new();
        self.ensure_schema(&mut chat, DocKind::Chat)
            .expect("create chat doc schema");
        self.ensure_schema(&mut session, DocKind::Session)
            .expect("create session doc schema");
        DocPair {
            chat,
            session,
            stream: StreamState::default(),
        }
    }

    /// 创建空 Registry Doc（全结构）。
    pub fn create_registry_doc(&self) -> yrs::Doc {
        let mut doc = yrs::Doc::new();
        self.ensure_schema(&mut doc, DocKind::Registry)
            .expect("create registry doc schema");
        doc
    }

    /// 幂等补结构：读根 Map `schema_version`——
    ///
    /// - 缺失 → 写入当前版本 + 全结构；
    /// - 小于/等于当前 → 检查必需键、缺失者补空结构（不覆盖已有数据）；
    /// - 大于当前 → [`FactoryError::FutureSchema`]。
    pub fn ensure_schema(&self, doc: &mut yrs::Doc, kind: DocKind) -> Result<(), FactoryError> {
        let current = self.version_for(kind);
        let mut txn = doc.transact_mut();
        let root = txn.get_or_insert_map(ROOT);

        let found = match root.get(&txn, "schema_version") {
            Some(yrs::Out::Any(yrs::Any::Number(n))) => Some(n as u32),
            _ => None,
        };

        match found {
            Some(v) if v > current => Err(FactoryError::FutureSchema {
                found: v,
                expected: current,
            }),
            Some(_) => {
                // 相等或旧版本：补缺失键，不覆盖已有。
                self.patch_missing(&mut txn, &root, kind);
                Ok(())
            }
            None => {
                // 缺失版本：写当前版本 + 全结构。
                root.insert(&mut txn, "schema_version", current);
                root.insert(&mut txn, "projection_version", 0u32);
                self.ensure_structure(&mut txn, &root, kind);
                Ok(())
            }
        }
    }

    fn version_for(&self, kind: DocKind) -> u32 {
        match kind {
            DocKind::Chat => self.chat_schema,
            DocKind::Session => self.session_schema,
            DocKind::Registry => self.registry_schema,
        }
    }

    /// 补齐缺失键（版本已存在路径；不覆盖已有数据）。
    fn patch_missing(&self, txn: &mut TransactionCtx<'_>, root: &yrs::MapRef, kind: DocKind) {
        if root.get(txn, "projection_version").is_none() {
            root.insert(txn, "projection_version", 0u32);
        }
        self.ensure_structure(txn, root, kind);
    }

    /// 保证结构性键存在（缺失者补空结构）。
    fn ensure_structure(&self, txn: &mut TransactionCtx<'_>, root: &yrs::MapRef, kind: DocKind) {
        match kind {
            DocKind::Chat => {
                if root.get(txn, "entry_order").is_none() {
                    root.insert(txn, "entry_order", yrs::ArrayPrelim::default());
                }
                if root.get(txn, "entries").is_none() {
                    root.insert(txn, "entries", yrs::MapPrelim::default());
                }
                if root.get(txn, "tool_calls").is_none() {
                    root.insert(txn, "tool_calls", yrs::MapPrelim::default());
                }
            }
            DocKind::Session => {
                if root.get(txn, "session").is_none() {
                    root.insert(txn, "session", yrs::MapPrelim::default());
                }
                if root.get(txn, "agent").is_none() {
                    root.insert(txn, "agent", yrs::MapPrelim::default());
                }
                if root.get(txn, "active_turn").is_none() {
                    root.insert(txn, "active_turn", yrs::Any::Null);
                }
                if root.get(txn, "pending_permissions").is_none() {
                    root.insert(txn, "pending_permissions", yrs::MapPrelim::default());
                }
                if root.get(txn, "sessions").is_none() {
                    root.insert(txn, "sessions", yrs::MapPrelim::default());
                }
            }
            DocKind::Registry => {
                if root.get(txn, "machines").is_none() {
                    root.insert(txn, "machines", yrs::MapPrelim::default());
                }
                if root.get(txn, "sessions").is_none() {
                    root.insert(txn, "sessions", yrs::MapPrelim::default());
                }
                if root.get(txn, "global").is_none() {
                    root.insert(
                        txn,
                        "global",
                        yrs::MapPrelim::from([("status", "healthy")]),
                    );
                }
            }
        }
    }
}

impl Default for Factory {
    fn default() -> Self {
        Self::new()
    }
}
