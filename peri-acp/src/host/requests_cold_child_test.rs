//! ACP 冷恢复（宿主态重建、同持久 store）的验收。
//!
//! 验证层级（准确层级，勿夸大）：**同进程内重建宿主态**——丢弃全部内存
//! `sessions` / 环境 / 父 Session，只凭持久 store 走真实宿主冷路径
//! `host::cold_execution::run`；不是 OS 进程重启，也不覆盖真实进程崩溃的
//! 其它持久事实（那归后续切片）。
//!
//! 断言面：冷恢复后发布会话状态里的 identity 只能来自 metadata 的版本锚定
//! 来源（v2 `identity_system` / v1 `persona`），子冻结 blob（创建时烘焙的父
//! 字节）不得成为身份；v1 缺锚必须拒绝执行且历史仍可读。

use super::*;
use peri_acp_types::frozen::FrozenRuntimeEnv;
use peri_acp_types::session_resources::work::{WorkAdmission, WorkCommand, WorkDecision};
use peri_acp_types::session_resources::{
    ChildSnapshot, FrozenSnapshotBytes, NewSession, NewSessionMeta,
};

use crate::host::SharedSessions;

const IDENTITY_V2: &str = "COLD_CHILD_IDENTITY_V2_SENTINEL";
const IDENTITY_V1: &str = "COLD_CHILD_PERSONA_V1_SENTINEL";
const CLAUDE: &str = "COLD_FROZEN_CLAUDE_SENTINEL";
const SKILLS: &str = "COLD_FROZEN_SKILLS_SENTINEL";

fn cold_workspace_assembly(cwd: &std::path::Path) -> crate::host::assemble::WorkspaceAssembly {
    crate::host::assemble::WorkspaceAssembly {
        startup_cwd: cwd.to_str().unwrap().to_owned(),
        bare: true,
        drive_cron_tick: false,
        mcp_profile: peri_middlewares::mcp::apps::McpCapabilityProfile::disabled(),
        capabilities: Default::default(),
    }
}

/// 从门面读出的根冻结字节的摘要（子会话冻结必须与根同源）。
fn frozen_digest(bytes: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes.as_bytes()))
}

fn child_metadata(anchor: ColdAnchor, model_name: &str, digest: String) -> serde_json::Value {
    let version = anchor.version();
    let persona = match anchor {
        ColdAnchor::V1Persona => Some(IDENTITY_V1),
        // 旧记录但写入方身份字段缺失：不可解释（不得用位置启发式猜）。
        ColdAnchor::V1Missing | ColdAnchor::V2Structured => None,
    };
    serde_json::json!({
        "version": version,
        "childSessionId": "",
        "recipientLifecycle": 1,
        "agentName": "cold-child",
        "modelName": model_name,
        "directInitiatorSessionId": "",
        "directInitiatorLifecycle": 1,
        "delegationInvocationId": "cold-invocation",
        "delegationTaskId": "cold-task",
        "authorizationRef": "cold-fixture-auth",
        "frozenDigest": digest,
        "toolCeiling": [],
        "toolOrigins": {},
        "skillNames": [],
        "maxIterations": 1,
        "persona": persona,
        "systemPrompt": "cold-fixture-parent-baked-system",
        "identitySystem": if version == 2 { Some(IDENTITY_V2) } else { None },
        "runtimeEnv": if version == 2 {
            Some(serde_json::to_value(FrozenRuntimeEnv {
                platform: "cold-fixture-os".into(),
                os_version: "cold-fixture-1".into(),
                is_git_repo: false,
            }).unwrap())
        } else { None },
        "claudeMd": CLAUDE,
        "claudeLocalMd": null,
        "skillSummary": SKILLS,
        "date": "1999-12-31",
        "language": null,
        "sectionOverrides": {},
        "disabledMiddlewares": [],
        "builtInSubagentsEnabled": false,
    })
}

/// 冷恢复夹具：真实 store 上建父根（session/new）与子会话持久事实。
struct ColdCase {
    cfg: Arc<AcpServerConfig>,
    child_id: String,
    child_lifecycle: u64,
    child_control_generation: u64,
    /// 根冻结 blob 里的父系统字节（子持久 blob 的烘焙内容；不是子身份来源）。
    parent_system: String,
    _tmp: tempfile::TempDir,
}

impl ColdCase {
    /// `persona` = None 时写 v1 但缺锚；`persona` = Some 写 v1 有锚；`v2` 写 v2。
    async fn build(anchor: ColdAnchor) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let _home = HomeDirGuard::set(tmp.path());
        let cwd = std::fs::canonicalize(tmp.path())
            .unwrap()
            .join("cold-project");
        std::fs::create_dir(&cwd).unwrap();
        let config =
            make_peri_config_with_provider(make_provider_config("test", "openai", "test", "model"));
        let model_name = anchor.model_name().to_owned();
        let provider = LlmProvider::OpenAi {
            api_key: "test-key".to_string(),
            base_url: "http://127.0.0.1:9/v1".to_string(),
            model: model_name.clone(),
            effort: None,
            max_tokens: 32000,
            context_1m: false,
            retry_observer: None,
        };
        let (mut cfg, _bridge) = make_server_config_with_bridge(config, provider, &tmp).await;
        cfg.workspace_assembly = Some(cold_workspace_assembly(&cwd));
        let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
        let mut sessions = HashMap::new();
        let parent = handle_request(
            "session/new",
            &json!({"cwd": cwd.to_str().unwrap()}),
            &cfg,
            &mut sessions,
            &transport,
        )
        .await
        .unwrap();
        let parent_id = parent["sessionId"].as_str().unwrap().to_owned();
        let parent_snapshot = cfg
            .session_resources
            .load_session_snapshot(&parent_id)
            .await
            .unwrap();
        let peri_acp_types::session_resources::BindingState::Bound(binding) =
            parent_snapshot.binding
        else {
            panic!("session/new 必须给出 bound 父根")
        };
        // 子会话冻结快照必须与根同源（真实持久事实：创建时烘焙的父冻结字节，
        // 其中 system 是父系统——正是 M3 禁止当子身份的那份数据）。
        let peri_acp_types::session_resources::FrozenState::Present(root_frozen) =
            parent_snapshot.frozen.clone()
        else {
            panic!("root must carry a frozen snapshot")
        };
        let root_frozen = root_frozen.into_string();
        let parent_system = crate::session::frozen_snapshot::decode_frozen_snapshot(&root_frozen)
            .unwrap()
            .system_prompt()
            .to_owned();
        let digest = frozen_digest(&root_frozen);
        let child_id = uuid::Uuid::now_v7().to_string();
        cfg.session_resources
            .save_child(&ChildSnapshot {
                target: NewSession {
                    thread_id: child_id.clone(),
                    created_at: peri_time::now_utc_rfc3339(),
                    meta: NewSessionMeta {
                        title: Some("cold-child".into()),
                        cwd: cwd.to_str().unwrap().to_owned(),
                        parent_thread_id: Some(parent_id.clone()),
                        hidden: true,
                        cancel_policy: Default::default(),
                        snapshot_at_message_id: None,
                    },
                    binding,
                    frozen: FrozenSnapshotBytes::new(root_frozen),
                },
                parent_id,
                root_id: parent_snapshot.meta.id.clone(),
                inherited: Default::default(),
            })
            .await
            .unwrap();
        let child_control = cfg
            .session_resources
            .load_session_control(&child_id)
            .await
            .unwrap();
        let lifecycle = child_control.lifecycle;
        // 子资源 owner 声明（冷环境装配前置；非空，避免 unknownBuiltin 阻断）。
        let revision = cfg
            .session_resources
            .load_work_revision(&child_id)
            .await
            .unwrap();
        let receipt = cfg
            .session_resources
            .apply_work_mutation(&WorkCommand {
                session_id: child_id.clone(),
                recipient_lifecycle: lifecycle,
                mutation_id: "cold-fixture-owners".into(),
                action: peri_acp_types::session_resources::work::WorkAction::BindResourceOwners {
                    expected_revision: revision,
                    connections_json:
                        "{\"workspace\":{\"url\":\"http://127.0.0.1:9/mcp\",\"system_mcp\":true}}"
                            .into(),
                    authorization_ref: "cold-fixture-auth".into(),
                },
            })
            .await
            .unwrap();
        assert_eq!(receipt.decision, WorkDecision::Accepted);
        // 版本化子恢复 metadata（v2 identity_system / v1 persona；直接对应写入方的
        // 确定字段——旧写入器写 persona，新写入器写 identity_system，不双写）。
        let mut metadata = child_metadata(anchor, &model_name, digest);
        metadata["childSessionId"] = json!(child_id);
        metadata["recipientLifecycle"] = json!(lifecycle);
        let revision = cfg
            .session_resources
            .load_work_revision(&child_id)
            .await
            .unwrap();
        let receipt = cfg
            .session_resources
            .apply_work_mutation(&WorkCommand {
                session_id: child_id.clone(),
                recipient_lifecycle: lifecycle,
                mutation_id: "cold-fixture-metadata".into(),
                action:
                    peri_acp_types::session_resources::work::WorkAction::BindChildResumeMetadata {
                        expected_revision: revision,
                        metadata_json: metadata.to_string(),
                    },
            })
            .await
            .unwrap();
        assert_eq!(receipt.decision, WorkDecision::Accepted);
        ColdCase {
            cfg: Arc::new(cfg),
            child_id,
            child_lifecycle: lifecycle,
            child_control_generation: child_control.control_generation,
            parent_system,
            _tmp: tmp,
        }
    }

    /// 宿主态重建：丢弃全部内存事实（sessions 清空），只保留同一个持久 store。
    fn rebuilt_sessions(&self) -> SharedSessions {
        Arc::new(tokio::sync::Mutex::new(HashMap::new()))
    }

    fn ticket(&self) -> WorkAdmission {
        serde_json::from_value(json!({
            "sessionId": self.child_id,
            "admissionId": "cold-fixture-admission",
            "instanceId": "cold-fixture-sdk",
            "generationId": uuid::Uuid::now_v7().to_string(),
            "lifecycle": self.child_lifecycle,
            "controlGeneration": self.child_control_generation,
            "workId": "cold-fixture-work",
            "workRevision": 0,
            "execution": {
                "turnId": "00000000-0000-4000-8000-000000000001",
                "attemptId": "cold-fixture-attempt",
            },
        }))
        .unwrap()
    }

    async fn cold_run(&self) -> (Result<(), String>, SharedSessions) {
        let sessions = self.rebuilt_sessions();
        let transport: Arc<dyn crate::transport::AcpTransport> = Arc::new(MockTransport::default());
        let outcome =
            crate::host::cold_execution::run(&self.ticket(), &self.cfg, &sessions, &transport)
                .await
                .map(|_| ())
                .map_err(|error| error.message);
        (outcome, sessions)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColdAnchor {
    V2Structured,
    V1Persona,
    V1Missing,
}

impl ColdAnchor {
    fn version(self) -> u32 {
        match self {
            Self::V2Structured => 2,
            Self::V1Persona | Self::V1Missing => 1,
        }
    }

    fn model_name(self) -> &'static str {
        "cold-capture-model"
    }
}

/// v2 冷恢复：状态身份 = metadata `identity_system`（不是持久 blob 的父字节），
/// 冻结贡献输入与 runtime 快照同源；历史可读。
#[tokio::test]
#[serial]
async fn cold_rebuild_publishes_child_identity_from_v2_metadata_not_the_parent_blob() {
    let case = ColdCase::build(ColdAnchor::V2Structured).await;
    let (outcome, sessions) = case.cold_run().await;
    let states = sessions.lock().await;
    let state = states
        .get(&case.child_id)
        .unwrap_or_else(|| panic!("冷恢复必须登记子会话状态: {outcome:?}"));
    let frozen = state.frozen.as_ref().expect("冷状态必须携带 frozen");
    assert_eq!(
        frozen.system_prompt(),
        IDENTITY_V2,
        "v2 冷恢复身份必须来自 metadata identity_system"
    );
    assert_ne!(
        frozen.system_prompt(),
        case.parent_system,
        "持久 blob 的父系统字节不得成为子身份"
    );
    assert_eq!(
        frozen.claude_md(),
        Some(CLAUDE),
        "冻结项目指令（贡献输入）同源"
    );
    assert_eq!(
        &*frozen.v2_frozen().skill_summary,
        SKILLS,
        "冻结技能摘要（贡献输入）同源"
    );
    // v2 metadata 的 runtime 快照随身份同源恢复（不重探宿主）。
    assert_eq!(
        frozen
            .v2_frozen()
            .runtime_env
            .as_ref()
            .map(|env| env.platform.as_str()),
        Some("cold-fixture-os"),
        "冻结运行环境必须来自 metadata 快照"
    );
    assert_eq!(
        state
            .history
            .iter()
            .map(|message| message.content())
            .collect::<Vec<_>>()
            .len(),
        state.history.len(),
        "历史投影可读"
    );
}

/// v1 有锚（persona）：可解释身份生效，blob 父字节仍不参与。
#[tokio::test]
#[serial]
async fn cold_rebuild_uses_explainable_v1_persona_anchor() {
    let case = ColdCase::build(ColdAnchor::V1Persona).await;
    let (outcome, sessions) = case.cold_run().await;
    let states = sessions.lock().await;
    let state = states
        .get(&case.child_id)
        .unwrap_or_else(|| panic!("v1 有锚冷恢复必须登记状态: {outcome:?}"));
    let frozen = state.frozen.as_ref().expect("冷状态必须携带 frozen");
    assert_eq!(
        frozen.system_prompt(),
        IDENTITY_V1,
        "v1 有锚身份必须来自写入方 persona"
    );
    assert_ne!(frozen.system_prompt(), case.parent_system);
}

/// v1 缺锚：明确拒绝执行恢复，不写任何会话状态；历史（持久载荷）仍可读。
#[tokio::test]
#[serial]
async fn cold_rebuild_refuses_v1_without_anchor_and_keeps_history_readable() {
    let case = ColdCase::build(ColdAnchor::V1Missing).await;
    let before = case
        .cfg
        .session_resources
        .load_session_snapshot(&case.child_id)
        .await
        .unwrap();
    let (outcome, sessions) = case.cold_run().await;
    let message = outcome.expect_err("v1 缺 persona 锚必须拒绝执行恢复");
    assert!(
        message.contains("no explainable identity source"),
        "必须报告身份不可解释: {message}"
    );
    assert!(
        sessions.lock().await.get(&case.child_id).is_none(),
        "拒绝执行时不得登记会话状态"
    );
    let after = case
        .cfg
        .session_resources
        .load_session_snapshot(&case.child_id)
        .await
        .unwrap();
    assert_eq!(
        before.payloads.len(),
        after.payloads.len(),
        "拒绝执行不得改动历史载荷"
    );
    assert!(
        after
            .meta
            .parent_thread_id
            .as_deref()
            .is_some_and(|parent| !parent.is_empty()),
        "历史元数据保持可读"
    );
}
