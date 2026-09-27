//! 远程会话写入行为：新建、fork、child、定向 metadata。
//!
//! 写入只有一种形状：**一次 `SessionDataPort` mutation = 一个托管事务批**，批内第一条
//! 是操作资格写入（见 [`super::ledger`]），其后才是业务效果。由此得到三条性质：
//!
//! - **整体生效或整体不生效**：中途失败整批回滚，不留半条会话；
//! - **重试不产生第二次效果**：操作 id 由 store 身份 + 语义标签 + 内容摘要定死，同一内容
//!   再次提交命中同一 id，读回原收据；不同内容不会互相冒充（摘要进了 id）；
//! - **未决不降级**：超时、网络失败、写忙都是「无法证明」，按未决上报而不是「未生效」。
//!
//! 写入前的核对（来源存在、父链根、root frozen 原文、父绑定一致）都是**不可变事实**：
//! 绑定创建时写一次、frozen 只有创建路径写、父关系创建后不再变，而本 adapter 不提供任何
//! 改绑/改父/改 frozen/删除的行为。因此这些读取不会在「读—写」之间失效，也不需要把核对
//! 塞进同一个事务来换一致性（远端也没有第二个写者能改写它们）。

use peri_acp_types::messages::MessageId;
use peri_acp_types::session_resources::{
    ChildSnapshot, ForkSnapshot, NewSession, SessionMetaPatch, SessionResourceError,
    SessionResourceErrorKind, SessionResourceResult,
};
use peri_acp_types::store::MessageFlags;
use peri_acp_types::thread::ThreadId;
use peri_acp_types::workspace::{SessionBinding, WorkspaceError};

use crate::sessions::data::ensure_child_relation;

use super::ledger::{OperationId, OperationIdentity};
use super::mutation::QualifiedMutation;
use super::session_codec::{self as codec, corrupt};
use super::session_data::{invalid_input, not_found, RemoteSessionData};
use super::session_sql::{self, SessionInsert};
use super::sql::StatementSpec;

impl RemoteSessionData {
    /// 保存新会话：meta + 不可变绑定 + frozen 完整落库（执行准入由本机执行面另行完成）。
    pub(super) async fn write_new_session(&self, input: &NewSession) -> SessionResourceResult<()> {
        // 远程新建只接受 **root**：带父的会话必须走 `save_child`，那里才有父子/根归属判定
        // （`data::ensure_child_relation`）、root owner 门禁与 frozen 继承。
        //
        // 这条判定原先挂在已撤销的远程执行面（`remote/local_execution.rs`）上，v10 撤销把
        // 那个文件连同它一起删掉了（真云回归因此转红：`cloud_limit_test.rs` 的
        // `cloud_remote_create_refuses_parent_input`）。判定只看纯输入、不读存储也不发请求，
        // 所以它必须在取连接之前给出——放行会往远端写一条**没有经过 child 通路判定**的父关系
        // （`session_insert` 照抄 `meta.parent_thread_id`，远端父链没有外键可依赖）。
        //
        // 注意这是**远程侧**的规则，不上升成门面的通用输入校验：本机 `save_new_session` 一直
        // 接受带父的目标，`sqlite_store` 侧的夹具依赖这一点（见本文件顶部的模块文档）。
        if input.meta.parent_thread_id.is_some() {
            return Err(invalid_input(
                "child sessions must be saved through the child path",
            ));
        }
        let statements =
            session_sql::insert_session_statements(&session_sql::session_insert(input, 0, None))?;
        let inputs = session_inputs(input);
        self.commit_effects("create_session", &inputs, statements, &input.thread_id)
            .await
            .map(|_| ())
    }

    /// 保存 fork：source 不变，目标带映射后的 payload 与 flags。
    pub(super) async fn write_fork(&self, fork: &ForkSnapshot) -> SessionResourceResult<()> {
        if fork.target.thread_id == fork.source_id {
            return Err(invalid_input("fork target must differ from its source"));
        }
        // flags 必须落在本批 payload 上，且 payload 内 id 不得重复：两者都在发请求前拒绝，
        // 因此批内不需要「更新了几行」式的后续核对。
        let mut ids: Vec<MessageId> = Vec::with_capacity(fork.payloads.len());
        for payload in &fork.payloads {
            if ids.contains(&payload.id()) {
                return Err(invalid_input("history batch repeats a message id"));
            }
            ids.push(payload.id());
        }
        for message_id in fork.flags.keys() {
            if !ids.contains(message_id) {
                return Err(invalid_input("fork flags reference an unknown message id"));
            }
        }
        if !self.exists(&fork.source_id).await? {
            return Err(not_found());
        }
        let insert = session_sql::session_insert(&fork.target, fork.payloads.len() as i64, None);
        let mut statements = session_sql::insert_session_statements(&insert)?;
        for payload in &fork.payloads {
            statements.push(session_sql::insert_message_statement(
                &fork.target.thread_id,
                payload,
                fork.flags.get(&payload.id()),
            )?);
        }
        let mut inputs = session_inputs(&fork.target);
        inputs.push(format!("source:{}", fork.source_id.as_str()));
        let id_list = ids
            .iter()
            .map(|id| id.as_uuid().to_string())
            .collect::<Vec<_>>()
            .join(",");
        inputs.push(id_list);
        // flags 进摘要必须带上投影内容：同一 message 的两种投影是两个不同的领域意图。
        let flag_list = fork
            .flags
            .iter()
            .map(|(id, flags)| flags_label(*id, flags))
            .collect::<Vec<_>>()
            .join(",");
        inputs.push(flag_list);
        self.commit_effects("fork_session", &inputs, statements, &fork.target.thread_id)
            .await
            .map(|_| ())
    }

    /// 保存 child：父子/根归属 + 继承区成立，frozen 逐字节取自 root 已保存的快照。
    pub(super) async fn write_child(&self, child: &ChildSnapshot) -> SessionResourceResult<()> {
        // 与门面、本机 adapter 共用同一条输入规则；它在任何远端读取之前生效，因此一次
        // 被拒的 child 既不发批、也不在本机日志里留下任何收据。
        ensure_child_relation(child)?;
        if !self.exists(&child.parent_id).await? {
            return Err(not_found());
        }
        let root = self.root_of(&child.parent_id).await?;
        if root != child.root_id {
            return Err(invalid_input("child root does not match its parent chain"));
        }
        if self.frozen_of(&child.root_id).await?.as_deref() != Some(child.target.frozen.as_str()) {
            return Err(invalid_input(
                "child frozen snapshot must be the root's saved snapshot",
            ));
        }
        // 子会话继承父会话的执行绑定身份，不另立 workspace 归属。
        if let Some(parent_binding) = self.binding_of(&child.parent_id).await? {
            if parent_binding != child.target.binding {
                return Err(SessionResourceError::new(
                    SessionResourceErrorKind::Workspace(WorkspaceError::ExecutionBindingMismatch),
                ));
            }
        }
        let inherited = codec::inherited_json(&child.inherited)?;
        let insert: SessionInsert<'_> =
            session_sql::session_insert(&child.target, 0, Some(inherited.as_str()));
        let statements = session_sql::insert_session_statements(&insert)?;
        let mut inputs = session_inputs(&child.target);
        inputs.push(format!("parent:{}", child.parent_id.as_str()));
        inputs.push(format!("root:{}", child.root_id.as_str()));
        inputs.push(inherited);
        // 子会话的根是显式给定的（已与父链核对一致），不需要再解析一次。
        self.commit_effects(
            "child_session",
            &inputs,
            statements,
            &child.target.thread_id,
        )
        .await
        .map(|_| ())
    }

    /// 定向 metadata 更新：没有字段要改时不写、也不假装更新了时间戳。
    pub(super) async fn write_meta(
        &self,
        id: &ThreadId,
        patch: &SessionMetaPatch,
    ) -> SessionResourceResult<()> {
        if patch.title.is_none()
            && patch.status.is_none()
            && patch.cancel_policy.is_none()
            && patch.config.is_none()
        {
            return Ok(());
        }
        let now = chrono::Utc::now().to_rfc3339();
        let title_input = patch_input(&patch.title);
        let status_input = patch
            .status
            .map(|status| status.as_str().to_owned())
            .unwrap_or_default();
        let policy_input = patch
            .cancel_policy
            .map(|policy| policy.as_str().to_owned())
            .unwrap_or_default();
        let config_input = patch_input(&patch.config);
        let inputs = vec![
            format!("id:{}", id.as_str()),
            format!("title:{title_input}"),
            format!("status:{status_input}"),
            format!("cancel_policy:{policy_input}"),
            format!("config:{config_input}"),
        ];
        let statements = vec![session_sql::update_meta_statement(id, patch, &now)];
        let counts = self
            .commit_effects("update_session_meta", &inputs, statements, id)
            .await?;
        match counts.first() {
            // 重放：原操作已生效，效果落在第一次提交里。
            None => Ok(()),
            Some(1) => Ok(()),
            Some(0) => Err(not_found()),
            Some(_) => Err(corrupt(
                "session metadata update did not apply to exactly one row",
            )),
        }
    }

    /// 一次「资格先于效果」的写入，返回效果语句的受影响行数。
    ///
    /// 顺序固定，且每一步都不能省：
    ///
    /// 1. **铸造身份**：本层给出一个全新的操作 id（不由内容派生，见 [`OperationIdentity`]），
    ///    输入摘要只用于事后一致性校验；
    /// 2. **发送**：一次托管事务批（资格先于效果），资格与效果同生共死；
    /// 3. **回报确定性**：已生效 / 确定未生效 / 无法证明三种结果原样上报，未知一律映射为
    ///    `PersistenceUncertain`，绝不折叠成「确定未生效」。
    ///
    /// 本机**不再**为这些操作留日志（v10 删除了 `session_remote_operations`，用户裁决不做
    /// 跨安装能力）：跨进程重启后没有「按原 id 向远端求证」这条路径，未结清只在本进程的
    /// 租约上表达，进程崩溃的未结清代际由 `execution_runs.clean = 0` 表达。
    pub(super) async fn commit_effects(
        &self,
        behavior: &str,
        inputs: &[String],
        effects: Vec<StatementSpec>,
        thread: &ThreadId,
    ) -> SessionResourceResult<Vec<u64>> {
        let identity = mint_identity(behavior, thread, inputs);
        let store = self.store().await?;
        let (outcome, counts) = store
            .apply_qualified_reporting(&QualifiedMutation { identity, effects })
            .await?;
        match outcome.failure_error(Some(thread)) {
            Some(error) => Err(error),
            None => Ok(counts),
        }
    }
}

/// 操作身份：每次调用唯一的 id，输入摘要只进身份做一致性校验。
///
/// 不复用调用方给的任何重试令牌，也不由内容派生 id：同一次领域调用连续发生两次，
/// 即使输入完全相同也是两次操作（状态 A→B→A、标题 x→y→x 都必须真的生效）。重试是
/// 新的一次操作，没有「按已落盘的原 id 恢复」这条路径（本机日志已随 v10 删除）。
fn mint_identity(behavior: &str, thread: &ThreadId, inputs: &[String]) -> OperationIdentity {
    OperationIdentity::new(OperationId::mint(thread), behavior, &input_labels(inputs))
}

fn input_labels(inputs: &[String]) -> Vec<&str> {
    inputs.iter().map(String::as_str).collect()
}

/// 定向更新的摘要输入：区分「不动」与「清除」，否则两种不同意图会撞同一个操作 id。
fn patch_input(slot: &Option<Option<String>>) -> String {
    match slot {
        None => "keep".to_owned(),
        Some(None) => "clear".to_owned(),
        Some(Some(value)) => format!("set:{value}"),
    }
}

/// 新建类输入（new / fork / child 的目标）的**完整**摘要输入。
///
/// 身份不由内容派生（id 每次唯一），摘要只用于事后一致性校验；但校验要成立，摘要就必须
/// 覆盖领域输入的全部字段：漏掉 binding/metadata 时，两次「内容摘要相同、实际写入不同」
/// 的操作会被判成同一次，那正是身份模型出错的表现，不能靠事后补字段掩盖。
fn session_inputs(input: &NewSession) -> Vec<String> {
    let meta = &input.meta;
    vec![
        format!("thread:{}", input.thread_id.as_str()),
        format!("created_at:{}", input.created_at),
        format!("title:{}", meta.title.as_deref().unwrap_or("<none>")),
        format!("cwd:{}", meta.cwd),
        format!(
            "parent:{}",
            meta.parent_thread_id.as_deref().unwrap_or("<none>")
        ),
        format!("hidden:{}", u8::from(meta.hidden)),
        format!("cancel_policy:{}", meta.cancel_policy.as_str()),
        format!(
            "snapshot_at:{}",
            meta.snapshot_at_message_id
                .as_ref()
                .map(|id| id.as_uuid().to_string())
                .unwrap_or_else(|| "<none>".to_owned())
        ),
        binding_input(&input.binding),
        format!("frozen:{}", input.frozen.as_str()),
    ]
}

/// 绑定进摘要：project/workspace/相对目录与绑定版本都给全，避免不同绑定的会话被判成同一操作。
fn binding_input(binding: &SessionBinding) -> String {
    format!(
        "binding:{}:{}:{}:{}",
        binding.schema_version,
        binding.project_id,
        binding.workspace_id,
        binding.cwd_relative_to_workspace.display()
    )
}

/// flags 进摘要：位与投影内容都要（同一 message 的不同投影是不同意图）。
pub(super) fn flags_label(message: MessageId, flags: &MessageFlags) -> String {
    format!(
        "{}:flags:{}{}{}:{}",
        message.as_uuid(),
        u8::from(flags.truncated),
        u8::from(flags.excluded),
        u8::from(flags.projection.is_some()),
        flags
            .projection
            .as_ref()
            .and_then(|projection| serde_json::to_string(projection).ok())
            .unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use peri_acp_types::session_resources::{NewSession, NewSessionMeta};
    use peri_acp_types::thread::CancelPolicy;
    use peri_acp_types::workspace::{ProjectId, SessionBinding, WorkspaceId};

    fn session(title: &str, workspace: WorkspaceId) -> NewSession {
        NewSession {
            thread_id: "thread-x".to_owned(),
            created_at: "2026-09-26T00:00:00Z".to_owned(),
            meta: NewSessionMeta {
                title: Some(title.to_owned()),
                cwd: "/tmp/synth".to_owned(),
                parent_thread_id: None,
                hidden: false,
                cancel_policy: CancelPolicy::Cascade,
                snapshot_at_message_id: None,
            },
            binding: SessionBinding {
                schema_version: 1,
                revision: 1,
                project_id: ProjectId::new(),
                workspace_id: workspace,
                cwd_relative_to_workspace: std::path::PathBuf::from("sub"),
            },
            frozen: peri_acp_types::session_resources::FrozenSnapshotBytes::new("{}"),
        }
    }

    /// 同内容连续三次领域调用必须得到三个不同操作 id（A→B→A、x→y→x 的根因）。
    #[test]
    fn identical_inputs_mint_distinct_operations() {
        let thread = ThreadId::from("thread-x");
        let inputs = vec!["status:done".to_owned()];
        let first = mint_identity("update_session_meta", &thread, &inputs);
        let second = mint_identity("update_session_meta", &thread, &inputs);
        let third = mint_identity("update_session_meta", &thread, &inputs);
        for (left, right) in [(&first, &second), (&second, &third), (&first, &third)] {
            assert_ne!(left.operation_id.as_str(), right.operation_id.as_str());
        }
        // 摘要只做一致性校验：同输入同摘要，异输入异摘要。
        assert_eq!(first.digest, third.digest);
        let other = mint_identity(
            "update_session_meta",
            &thread,
            &["status:active".to_owned()],
        );
        assert_ne!(first.digest, other.digest);
        // 身份不带内容原文，只带会话定位前缀。
        assert!(!first.operation_id.as_str().contains("done"));
        assert!(first.operation_id.as_str().starts_with("thread-x."));
    }

    /// 新建摘要必须覆盖 binding 与 metadata：只差其中一个字段就是两次不同的操作。
    #[test]
    fn session_inputs_cover_binding_and_metadata() {
        let base = session("title-a", WorkspaceId::new());
        let other_title = session("title-b", base.binding.workspace_id);
        let mut other_binding = base.clone();
        other_binding.binding.workspace_id = WorkspaceId::new();

        let base_inputs = session_inputs(&base);
        let title_inputs = session_inputs(&other_title);
        let binding_inputs = session_inputs(&other_binding);
        assert_ne!(base_inputs.join(""), title_inputs.join(""));
        assert_ne!(base_inputs.join(""), binding_inputs.join(""));
    }
}
