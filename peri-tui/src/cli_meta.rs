use super::MachineAction;
use anyhow::{Context, Result, bail, ensure};
use peri_acp_types::session_resources::{SessionResourceError, SessionResourceErrorKind};
use peri_acp_types::session_store::SessionStoreDeployment;
use peri_acp_types::thread::{ThreadId, ThreadMeta};
use peri_acp_types::workspace::MachineIdentityKind;
use peri_acp_types::workspace::{MachineInfo, WorkspaceInfo};
use peri_resources::{StoreOpenFailure, classify_open_failure};
use serde::Serialize;
use uuid::Uuid;

pub(crate) async fn run_machine_adopt(
    action: MachineAction,
    deployment: SessionStoreDeployment,
) -> Result<()> {
    let MachineAction::Adopt {
        target,
        current,
        apply,
        confirm_no_active_executions,
    } = action;
    let target = Uuid::parse_str(&target)
        .context("target Machine ID must be a UUID")?
        .to_string();
    let resources = peri_resources::Resources::open_deployment(&deployment).await?;
    let sessions = resources.session_resources();
    let current_id = peri_resources::sessions::current_machine_id()?.to_owned();
    let machines = sessions.list_machines().await?;
    let selected = machines
        .iter()
        .find(|machine| machine.id == target)
        .context("target Machine ID is not registered in this store")?;
    ensure!(
        selected.identity_kind == MachineIdentityKind::Known,
        "legacy unknown identity cannot be adopted"
    );
    let workspaces = sessions.list_workspaces(&target).await?;
    println!("Current Machine ID: {current_id}");
    println!("Adopt: {} ({target})", selected.name.escape_debug());
    for workspace in workspaces {
        println!(
            "  {}  {}",
            workspace.id,
            workspace.path.to_string_lossy().escape_debug()
        );
    }
    if !apply {
        println!(
            "Review this identity, then rerun with --current {current_id} --apply --confirm-no-active-executions."
        );
        return Ok(());
    }
    ensure!(
        confirm_no_active_executions,
        "--apply requires --confirm-no-active-executions"
    );
    let Some(expected) = current else {
        bail!("--apply requires --current with the displayed Machine ID");
    };
    ensure!(
        Uuid::parse_str(&expected)?.to_string() == current_id,
        "current Machine ID changed; review the catalog again"
    );
    peri_resources::sessions::adopt_file_identity(&current_id, &target)?;
    println!("Machine identity saved. Restart Peri to use {target}.");
    Ok(())
}

const SCHEMA_VERSION: u8 = 1;

pub(crate) struct MetaCommandOutcome {
    pub(crate) stdout: Option<String>,
    pub(crate) stderr: Option<String>,
    pub(crate) exit_code: u8,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionMetaDtoV1 {
    schema_version: u8,
    id: String,
    title: Option<String>,
    cwd: String,
    created_at: String,
    updated_at: String,
    message_count: usize,
    parent_thread_id: Option<String>,
    persisted_agent_status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MachineCatalogEntry {
    #[serde(flatten)]
    machine: MachineInfo,
    workspaces: Vec<WorkspaceInfo>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MachineCatalogDto {
    current_machine_id: String,
    machines: Vec<MachineCatalogEntry>,
}

pub(crate) async fn run_meta_machines(
    deployment: SessionStoreDeployment,
    json: bool,
) -> MetaCommandOutcome {
    let resources = match peri_resources::Resources::open_deployment(&deployment).await {
        Ok(resources) => resources,
        Err(error) => {
            return error_outcome_with_message(map_open_error(&error), json, format!("{error:#}"));
        }
    };
    let sessions = resources.session_resources();
    let current_machine_id = match peri_resources::sessions::current_machine_id() {
        Ok(id) => id.to_owned(),
        Err(error) => {
            return error_outcome_with_message(
                MetaErrorKind::InternalError,
                json,
                error.to_string(),
            );
        }
    };
    let machines = match sessions.list_machines().await {
        Ok(machines) => machines,
        Err(error) => {
            return error_outcome_with_message(map_resource_error(&error), json, error.to_string());
        }
    };
    let mut entries = Vec::with_capacity(machines.len());
    for machine in machines {
        let workspaces = match sessions.list_workspaces(&machine.id).await {
            Ok(workspaces) => workspaces,
            Err(error) => {
                return error_outcome_with_message(
                    map_resource_error(&error),
                    json,
                    error.to_string(),
                );
            }
        };
        entries.push(MachineCatalogEntry {
            machine,
            workspaces,
        });
    }
    let output = if json {
        match serde_json::to_string(&MachineCatalogDto {
            current_machine_id: current_machine_id.clone(),
            machines: entries,
        }) {
            Ok(output) => output,
            Err(error) => {
                return error_outcome_with_message(
                    MetaErrorKind::InternalError,
                    true,
                    error.to_string(),
                );
            }
        }
    } else {
        let mut output = format!("Current Machine ID: {current_machine_id}\n");
        for entry in entries {
            use std::fmt::Write;
            let current = if entry.machine.is_current { " *" } else { "" };
            let _ = writeln!(
                output,
                "{} ({}){}",
                escape_human(&entry.machine.name),
                entry.machine.id,
                current
            );
            for workspace in entry.workspaces {
                let _ = writeln!(
                    output,
                    "  {}  {}",
                    workspace.id,
                    escape_human(&workspace.path.to_string_lossy())
                );
            }
        }
        output.trim_end_matches('\n').to_owned()
    };
    MetaCommandOutcome {
        stdout: Some(format!("{output}\n")),
        stderr: None,
        exit_code: 0,
    }
}

impl From<ThreadMeta> for SessionMetaDtoV1 {
    fn from(meta: ThreadMeta) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            id: meta.id,
            title: meta.title,
            cwd: meta.cwd,
            created_at: meta.created_at.to_rfc3339(),
            updated_at: meta.updated_at.to_rfc3339(),
            message_count: meta.message_count,
            parent_thread_id: meta.parent_thread_id,
            persisted_agent_status: meta.agent_status.as_str().to_owned(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum MetaErrorKind {
    InvalidArgument,
    InvalidSessionId,
    DatabaseNotFound,
    DatabaseUnreadable,
    SchemaIncompatible,
    SessionNotFound,
    CorruptSessionData,
    /// 存储定位/凭证来源等配置错误（缺配置、互斥、未知引擎）。
    StoreNotConfigured,
    /// 存储后端不可用（远程 adapter 未接线、连接失败）。
    StoreUnavailable,
    InternalError,
}

impl MetaErrorKind {
    fn name(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid_argument",
            Self::InvalidSessionId => "invalid_session_id",
            Self::DatabaseNotFound => "database_not_found",
            Self::DatabaseUnreadable => "database_unreadable",
            Self::SchemaIncompatible => "schema_incompatible",
            Self::SessionNotFound => "session_not_found",
            Self::CorruptSessionData => "corrupt_session_data",
            Self::StoreNotConfigured => "store_not_configured",
            Self::StoreUnavailable => "store_unavailable",
            Self::InternalError => "internal_error",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::InvalidArgument => "invalid Meta command arguments",
            Self::InvalidSessionId => "session ID must be a valid UUID",
            Self::DatabaseNotFound => "thread database was not found",
            Self::DatabaseUnreadable => "thread database could not be opened for reading",
            Self::SchemaIncompatible => "thread database schema is incompatible",
            Self::SessionNotFound => "session was not found",
            Self::CorruptSessionData => "stored session metadata is corrupt",
            Self::StoreNotConfigured => "session store configuration is invalid",
            Self::StoreUnavailable => "session store is currently unavailable",
            Self::InternalError => "an internal error occurred",
        }
    }

    fn exit_code(self) -> u8 {
        match self {
            Self::InternalError => 1,
            Self::InvalidArgument | Self::InvalidSessionId | Self::StoreNotConfigured => 2,
            Self::DatabaseNotFound | Self::SessionNotFound => 3,
            Self::DatabaseUnreadable
            | Self::SchemaIncompatible
            | Self::CorruptSessionData
            | Self::StoreUnavailable => 4,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MetaErrorDtoV1 {
    schema_version: u8,
    error: MetaErrorBodyV1,
}

#[derive(Serialize)]
struct MetaErrorBodyV1 {
    kind: &'static str,
    message: String,
}

pub(crate) fn invalid_argument_outcome(json: bool) -> MetaCommandOutcome {
    error_outcome(MetaErrorKind::InvalidArgument, json)
}

pub(crate) fn internal_error_outcome(json: bool) -> MetaCommandOutcome {
    error_outcome(MetaErrorKind::InternalError, json)
}

pub(crate) async fn run_meta_session(
    deployment: SessionStoreDeployment,
    session_id: String,
    json: bool,
) -> MetaCommandOutcome {
    // Validate syntax before resolving or opening any database. Keep the original
    // spelling for lookup because ThreadId is persisted as text.
    if Uuid::parse_str(&session_id).is_err() {
        return error_outcome(MetaErrorKind::InvalidSessionId, json);
    }

    // 统一只读入口：与 TUI/print/stdio 共享同一个 typed open request 与后端选择点，
    // 访问意图在入口处固定为只读（不写任何本机文件、不登记 owner、不建目录）。
    let resources = match peri_resources::Resources::open_deployment(&deployment).await {
        Ok(resources) => resources,
        Err(error) => {
            return error_outcome_with_message(map_open_error(&error), json, format!("{error:#}"));
        }
    };
    let meta = match resources
        .session_resources()
        .load_session_meta(&ThreadId::from(session_id))
        .await
    {
        Ok(meta) => meta,
        Err(error) => {
            return error_outcome_with_message(map_resource_error(&error), json, error.to_string());
        }
    };

    success_outcome(SessionMetaDtoV1::from(meta), json)
}

/// 门面读取失败按既有只读命令语义分类：只读打开已经成功，因此这里的失败只表达
/// 「这条会话查不到」「库内容读不懂」或「库此刻读不了」，其余一律归内部错误。
fn map_resource_error(error: &SessionResourceError) -> MetaErrorKind {
    match error.kind() {
        SessionResourceErrorKind::NotFound => MetaErrorKind::SessionNotFound,
        SessionResourceErrorKind::Corrupt { .. } => MetaErrorKind::CorruptSessionData,
        SessionResourceErrorKind::Unavailable { .. } => MetaErrorKind::DatabaseUnreadable,
        _ => MetaErrorKind::InternalError,
    }
}

fn map_open_error(error: &anyhow::Error) -> MetaErrorKind {
    match classify_open_failure(error) {
        StoreOpenFailure::NotConfigured => MetaErrorKind::StoreNotConfigured,
        StoreOpenFailure::NotFound => MetaErrorKind::DatabaseNotFound,
        StoreOpenFailure::Unreadable => MetaErrorKind::DatabaseUnreadable,
        StoreOpenFailure::SchemaIncompatible => MetaErrorKind::SchemaIncompatible,
        StoreOpenFailure::Corrupt => MetaErrorKind::CorruptSessionData,
        StoreOpenFailure::Unavailable => MetaErrorKind::StoreUnavailable,
        StoreOpenFailure::Internal => MetaErrorKind::InternalError,
    }
}

fn success_outcome(dto: SessionMetaDtoV1, json: bool) -> MetaCommandOutcome {
    let output = if json {
        match serde_json::to_string(&dto) {
            Ok(value) => value,
            Err(error) => {
                return error_outcome_with_message(
                    MetaErrorKind::InternalError,
                    true,
                    error.to_string(),
                );
            }
        }
    } else {
        render_human(&dto)
    };
    MetaCommandOutcome {
        stdout: Some(format!("{output}\n")),
        stderr: None,
        exit_code: 0,
    }
}

fn error_outcome(kind: MetaErrorKind, json: bool) -> MetaCommandOutcome {
    error_outcome_with_message(kind, json, kind.message().to_owned())
}

fn error_outcome_with_message(
    kind: MetaErrorKind,
    json: bool,
    message: String,
) -> MetaCommandOutcome {
    let message = peri_acp_types::session::bounded_error_message(&message, 2_000);
    tracing::error!(kind = kind.name(), error = %message, "Meta command failed");
    let output = if json {
        let dto = MetaErrorDtoV1 {
            schema_version: SCHEMA_VERSION,
            error: MetaErrorBodyV1 {
                kind: kind.name(),
                message: message.clone(),
            },
        };
        serde_json::to_string(&dto).expect("Meta error DTO contains only strings and an integer")
    } else {
        format!("{}: {}", kind.name(), message)
    };
    MetaCommandOutcome {
        stdout: None,
        stderr: Some(format!("{output}\n")),
        exit_code: kind.exit_code(),
    }
}

fn render_human(dto: &SessionMetaDtoV1) -> String {
    format!(
        "Schema version: {}\nID: {}\nTitle: {}\nCWD: {}\nCreated at: {}\nUpdated at: {}\nMessage count: {}\nParent thread ID: {}\nPersisted agent status: {}",
        dto.schema_version,
        escape_human(&dto.id),
        render_nullable(dto.title.as_deref()),
        escape_human(&dto.cwd),
        escape_human(&dto.created_at),
        escape_human(&dto.updated_at),
        dto.message_count,
        render_nullable(dto.parent_thread_id.as_deref()),
        escape_human(&dto.persisted_agent_status),
    )
}

fn render_nullable(value: Option<&str>) -> String {
    value.map(escape_human).unwrap_or_else(|| "null".to_owned())
}

fn escape_human(value: &str) -> String {
    value.chars().flat_map(char::escape_debug).collect()
}

#[cfg(test)]
#[path = "cli_meta_test.rs"]
mod tests;
