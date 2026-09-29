//! `workspace` handler 的资源面 wire 证据（W1 验收）：真实 rmcp 内存链路
//! （`serve_server` + Auto lifecycle client），server 半边是生产 handler。
//!
//! 覆盖验收列：能力位与实际方法一致、`list` 每项可读、`skills/get` 局部列表外项
//! 可读、未知 `-32602`、工具表仍七项（无 SkillTool）、blob/meta 投影与
//! 「未装配 provider 的诚实语义」。

use std::time::Duration;

use base64::Engine as _;
use peri_acp_types::workspace_resources::{
    ResourceScope, SkillsListResponse, META_KEY_DIGEST, META_KEY_SCOPE, SKILL_ENTRY_FILE,
};
use rmcp::{
    model::{
        CustomRequest, ErrorCode, PaginatedRequestParams, ReadResourceRequestParams,
        ReadResourceResult, ResourceContents, ServerResult,
    },
    service::{Peer, QuitReason, RunningService},
    ClientLifecycleMode, RoleClient,
};

use crate::resources::{
    ResourceBudget, ResourceRoot, WorkspaceResourceProvider, WorkspaceResourcesInput,
};
use crate::WorkspaceMcpServer;

const DUPLEX_BUF: usize = 8 * 1024;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("临时目录夹具必须可创建")
}

fn write_skill(root: &std::path::Path, dir_name: &str, name: &str) {
    let dir = root.join(dir_name);
    std::fs::create_dir_all(&dir).expect("建技能目录");
    std::fs::write(
        dir.join(SKILL_ENTRY_FILE),
        format!("---\nname: {name}\ndescription: wire demo\n---\nbody\n"),
    )
    .expect("写 SKILL.md");
}

/// 装配了资源面的 server：project 技能根（alpha + 隐藏的重复项）与工作区指令。
fn server_with_resources(cwd: &std::path::Path, skills: &std::path::Path) -> WorkspaceMcpServer {
    std::fs::write(cwd.join("CLAUDE.md"), "project instructions\n").expect("写指令");
    write_skill(skills, "alpha", "alpha");
    std::fs::write(skills.join("alpha").join("data.bin"), [0x00, 0xff, 0x01])
        .expect("写二进制附件");
    WorkspaceMcpServer::new(cwd.to_string_lossy().to_string(), None).with_resources(
        WorkspaceResourcesInput {
            skill_roots: vec![ResourceRoot::new(skills, ResourceScope::Project)],
            agent_roots: Vec::new(),
            disable_bundled: true,
            budget: ResourceBudget::default(),
        },
    )
}

struct Pair {
    service: RunningService<RoleClient, ()>,
    server_task: tokio::task::JoinHandle<Result<QuitReason, tokio::task::JoinError>>,
}

impl Pair {
    fn peer(&self) -> Peer<RoleClient> {
        self.service.peer().clone()
    }

    async fn shutdown(mut self) {
        let _ = self.service.close_with_timeout(CLOSE_TIMEOUT).await;
        if tokio::time::timeout(CLOSE_TIMEOUT, &mut self.server_task)
            .await
            .is_err()
        {
            self.server_task.abort();
            let _ = self.server_task.await;
        }
    }
}

async fn connect(server: WorkspaceMcpServer) -> Pair {
    connect_with(
        server,
        ClientLifecycleMode::Auto {
            preferred_versions: vec![rmcp::model::ProtocolVersion::V_2026_07_28],
            legacy_version: None,
        },
    )
    .await
}

/// 以 legacy 协议版本握手（验证 SEP-2164 的版本相关错误码）。
async fn connect_legacy(server: WorkspaceMcpServer) -> Pair {
    connect_with(
        server,
        ClientLifecycleMode::Auto {
            preferred_versions: vec![rmcp::model::ProtocolVersion::V_2025_11_25],
            legacy_version: None,
        },
    )
    .await
}

async fn connect_with(server: WorkspaceMcpServer, lifecycle: ClientLifecycleMode) -> Pair {
    let (client_io, server_io) = tokio::io::duplex(DUPLEX_BUF);
    let (read, write) = tokio::io::split(server_io);
    let server_task = tokio::spawn(async move {
        let running = rmcp::serve_server(server, (read, write))
            .await
            .expect("workspace server 装配失败");
        running.waiting().await
    });
    let service = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        rmcp::serve_client_with_lifecycle((), tokio::io::split(client_io), lifecycle),
    )
    .await
    .expect("client 侧握手超时")
    .expect("client 侧握手失败");
    Pair {
        service,
        server_task,
    }
}

async fn skills_list(peer: &Peer<RoleClient>, params: Option<serde_json::Value>) -> ServerResult {
    let request = CustomRequest::new("skills/list", params);
    peer.send_request(rmcp::model::ClientRequest::CustomRequest(request))
        .await
        .expect("skills/list 必须成功")
}

async fn skills_get(peer: &Peer<RoleClient>, uri: &str) -> Result<ServerResult, String> {
    let request = CustomRequest::new("skills/get", Some(serde_json::json!({ "uri": uri })));
    peer.send_request(rmcp::model::ClientRequest::CustomRequest(request))
        .await
        .map_err(|error| error.to_string())
}

fn custom_value(result: ServerResult) -> serde_json::Value {
    match result {
        ServerResult::CustomResult(custom) => custom.0,
        other => panic!("期望 CustomResult，实际：{other:?}"),
    }
}

fn mcp_error(error: rmcp::ServiceError) -> rmcp::model::ErrorData {
    match error {
        rmcp::ServiceError::McpError(data) => data,
        other => panic!("期望 MCP 协议错误，实际：{other:?}"),
    }
}

fn single_contents(result: ReadResourceResult) -> ResourceContents {
    assert_eq!(result.contents.len(), 1, "本波资源读取恒返回单条 contents");
    result.contents.into_iter().next().expect("非空")
}

// ─── 能力位与工具面 ───────────────────────────────────────────────────────────

#[tokio::test]
async fn wire_declares_resources_and_keeps_seven_tools() {
    let cwd = tempdir();
    let skills = tempdir();
    let pair = connect(server_with_resources(cwd.path(), skills.path())).await;

    let tools = pair.peer().list_tools(None).await.expect("tools/list");
    let names: Vec<String> = tools
        .tools
        .iter()
        .map(|tool| tool.name.to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "Read",
            "Write",
            "Edit",
            "Glob",
            "Grep",
            "folder_operations",
            "Bash"
        ],
        "W1 不得改变七工具声明顺序，也不得新增 SkillTool"
    );

    // 能力位与实际方法一致：声明 resources 后 list/read/templates 都必须可达。
    let resources = pair
        .peer()
        .list_resources(None)
        .await
        .expect("resources/list");
    assert!(
        resources
            .resources
            .iter()
            .any(|resource| resource.uri == "workspace://git/ref"),
        "既有 git ref 资源必须保留"
    );
    let templates = pair
        .peer()
        .list_resource_templates(None)
        .await
        .expect("resources/templates/list");
    assert!(!templates.resource_templates.is_empty());

    pair.shutdown().await;
}

// ─── list 每项可读 ────────────────────────────────────────────────────────────

#[tokio::test]
async fn wire_every_listed_uri_is_readable() {
    let cwd = tempdir();
    let skills = tempdir();
    let pair = connect(server_with_resources(cwd.path(), skills.path())).await;

    let resources = pair
        .peer()
        .list_resources(None)
        .await
        .expect("resources/list");
    let uris: Vec<String> = resources
        .resources
        .iter()
        .map(|resource| resource.uri.clone())
        .collect();
    assert!(uris.contains(&"skill://project/alpha/SKILL.md".to_string()));
    assert!(uris.contains(&"skill://project/alpha/data.bin".to_string()));
    assert!(uris.contains(&"peri-instruction://workspace/main".to_string()));
    assert!(uris.contains(&"peri-instruction://workspace/index".to_string()));

    for uri in &uris {
        pair.peer()
            .read_resource(ReadResourceRequestParams::new(uri.clone()))
            .await
            .unwrap_or_else(|error| panic!("list 项必须可读：{uri}（{error}）"));
    }
    pair.shutdown().await;
}

#[tokio::test]
async fn wire_blob_and_meta_projection() {
    let cwd = tempdir();
    let skills = tempdir();
    let pair = connect(server_with_resources(cwd.path(), skills.path())).await;

    let result = pair
        .peer()
        .read_resource(ReadResourceRequestParams::new(
            "skill://project/alpha/data.bin",
        ))
        .await
        .expect("blob 读取");
    let contents = single_contents(result);
    match &contents {
        ResourceContents::BlobResourceContents {
            blob, mime_type, ..
        } => {
            assert_eq!(
                blob,
                &base64::engine::general_purpose::STANDARD.encode([0x00, 0xff, 0x01]),
                "blob 必须是原始字节的 base64"
            );
            assert_eq!(mime_type.as_deref(), Some("application/octet-stream"));
        }
        other => panic!("二进制附件必须是 blob 内容：{other:?}"),
    }
    // `_meta` 带 scope 与 digest（W1 冻结的 io.peri/ 字段）。
    if let ResourceContents::BlobResourceContents {
        meta: Some(meta), ..
    } = &contents
    {
        assert_eq!(
            meta.0.get(META_KEY_SCOPE).and_then(|value| value.as_str()),
            Some("project")
        );
        let digest = meta
            .0
            .get(META_KEY_DIGEST)
            .and_then(|value| value.as_str())
            .expect("digest");
        assert_eq!(
            digest,
            peri_acp_types::workspace_resources::digest_bytes(&[0x00, 0xff, 0x01]),
            "read 的 digest 与内容同源"
        );
    } else {
        panic!("blob 必须带 _meta");
    }

    // 文本附件走 Text 通道。
    let result = pair
        .peer()
        .read_resource(ReadResourceRequestParams::new(
            "skill://project/alpha/SKILL.md",
        ))
        .await
        .expect("文本读取");
    match single_contents(result) {
        ResourceContents::TextResourceContents {
            text, mime_type, ..
        } => {
            assert!(text.contains("name: alpha"));
            assert_eq!(mime_type.as_deref(), Some("text/markdown"));
        }
        other => panic!("SKILL.md 必须是文本内容：{other:?}"),
    }

    pair.shutdown().await;
}

// ─── skills/list|get 与错误码 ────────────────────────────────────────────────

#[tokio::test]
async fn wire_skills_list_and_get_semantics() {
    let cwd = tempdir();
    let skills = tempdir();
    // 高优先级根（project）与低优先级根（user）同名：list 只列 winner，shadow 仍可 get。
    let shadow = tempdir();
    write_skill(shadow.path(), "alpha", "alpha");
    std::fs::write(shadow.path().join("alpha").join("SKILL.md"), {
        // shadow 的技能内容不同，证明 get 按 URI 直读而非复用 winner。
        "---\nname: alpha\ndescription: shadow copy\n---\nshadow body\n"
    })
    .expect("写 shadow");

    let server = WorkspaceMcpServer::new(cwd.path().to_string_lossy().to_string(), None)
        .with_resources(WorkspaceResourcesInput {
            skill_roots: vec![
                ResourceRoot::new(skills.path(), ResourceScope::Project),
                ResourceRoot::new(shadow.path(), ResourceScope::User),
            ],
            agent_roots: Vec::new(),
            disable_bundled: true,
            budget: ResourceBudget::default(),
        });
    write_skill(skills.path(), "alpha", "alpha");
    std::fs::write(cwd.path().join("CLAUDE.md"), "instructions\n").expect("写指令");
    let pair = connect(server).await;

    // list：唯一命中 project 的 alpha（user 的 shadow 不公开）。
    let value = custom_value(skills_list(&pair.peer(), None).await);
    let response: SkillsListResponse = serde_json::from_value(value).expect("skills/list 形状");
    assert_eq!(response.skills.len(), 1, "shadow 项不出现在列表");
    let entry = &response.skills[0];
    assert_eq!(entry.uri, "skill://project/alpha/SKILL.md");
    assert_eq!(
        entry.frontmatter.get("description"),
        Some(&serde_json::json!("wire demo"))
    );
    let manifest = entry.resources.as_ref().expect("完整 manifest");
    assert_eq!(manifest[0].uri, "skill://project/alpha/SKILL.md");
    assert!(manifest[0].digest.starts_with("sha256:"));
    // manifest 的每个 URI 都能经 resources/read 读到。
    for resource in manifest {
        pair.peer()
            .read_resource(ReadResourceRequestParams::new(resource.uri.clone()))
            .await
            .unwrap_or_else(|error| panic!("manifest 项必须可读：{}（{error}）", resource.uri));
    }

    // get 局部列表外项（shadow 的 URI 未被 list 枚举，但可按 URI 读取当前快照）。
    let value = custom_value(
        skills_get(&pair.peer(), "skill://user/alpha/SKILL.md")
            .await
            .expect("shadow get 必须成功"),
    );
    let get: peri_acp_types::workspace_resources::SkillGetResponse =
        serde_json::from_value(value).expect("skills/get 形状");
    assert_eq!(get.skill.uri, "skill://user/alpha/SKILL.md");
    assert_eq!(
        get.skill.frontmatter.get("description"),
        Some(&serde_json::json!("shadow copy"))
    );

    // 未知 URI → -32602（MCPP 约定）。
    let error = skills_get(&pair.peer(), "skill://project/nope/SKILL.md")
        .await
        .expect_err("未知技能必须报错");
    assert!(
        error.contains("-32602"),
        "skills/get 未知 URI 必须是 -32602：{error}"
    );
    // 非空 cursor（首期不分页）→ -32602。
    let failure = pair
        .peer()
        .send_request(rmcp::model::ClientRequest::CustomRequest(
            CustomRequest::new("skills/list", Some(serde_json::json!({ "cursor": "next" }))),
        ))
        .await
        .expect_err("非空 cursor 必须拒绝");
    assert_eq!(mcp_error(failure).code.0, ErrorCode::INVALID_PARAMS.0);
    // 未知 custom method → -32601。
    let failure = pair
        .peer()
        .send_request(rmcp::model::ClientRequest::CustomRequest(
            CustomRequest::new("skills/other", None),
        ))
        .await
        .expect_err("未知方法必须拒绝");
    assert_eq!(mcp_error(failure).code.0, ErrorCode::METHOD_NOT_FOUND.0);

    pair.shutdown().await;
}

#[tokio::test]
async fn wire_resource_read_error_codes() {
    let cwd = tempdir();
    let skills = tempdir();
    let pair = connect(server_with_resources(cwd.path(), skills.path())).await;

    // 合法 URI、目标缺失：handler 返回 RESOURCE_NOT_FOUND；2026-07-28 协商下 SDK
    // 按 SEP-2164 升级为 -32602（message 仍可区分）。
    let failure = pair
        .peer()
        .read_resource(ReadResourceRequestParams::new(
            "skill://project/nope/SKILL.md",
        ))
        .await
        .expect_err("缺失技能必须报错");
    let error = mcp_error(failure);
    assert_eq!(
        error.code.0,
        ErrorCode::INVALID_PARAMS.0,
        "SEP-2164：新协议版本下资源不存在升级为 -32602；message: {}",
        error.message
    );
    assert_eq!(error.message.as_ref(), "resource not found");

    // URI 非法 / scheme 未知 → -32602（handler 直接给）。
    let failure = pair
        .peer()
        .read_resource(ReadResourceRequestParams::new("HTTP://example.test/x"))
        .await
        .expect_err("未知 scheme 必须报错");
    assert_eq!(mcp_error(failure).code.0, ErrorCode::INVALID_PARAMS.0);

    // git ref 既有路径不受影响。
    pair.peer()
        .read_resource(ReadResourceRequestParams::new("workspace://git/ref"))
        .await
        .expect("git ref 必须仍可读");

    pair.shutdown().await;

    // legacy 协商（2025-11-25）：同一缺失保留 -32002（SEP-2164 的旧版口径）。
    let cwd = tempdir();
    let skills = tempdir();
    let legacy = connect_legacy(server_with_resources(cwd.path(), skills.path())).await;
    let failure = legacy
        .peer()
        .read_resource(ReadResourceRequestParams::new(
            "skill://project/nope/SKILL.md",
        ))
        .await
        .expect_err("缺失技能必须报错");
    assert_eq!(
        mcp_error(failure).code.0,
        ErrorCode::RESOURCE_NOT_FOUND.0,
        "旧协议版本保持 -32002"
    );
    legacy.shutdown().await;
}

// ─── 未装配 provider 的诚实语义 ──────────────────────────────────────────────

#[tokio::test]
async fn wire_unwired_provider_is_honest() {
    let cwd = tempdir();
    let pair = connect(WorkspaceMcpServer::new(
        cwd.path().to_string_lossy().to_string(),
        None,
    ))
    .await;

    // 资源发现面与本波之前一致：只有 git ref。
    let resources = pair
        .peer()
        .list_resources(None)
        .await
        .expect("resources/list");
    assert_eq!(resources.resources.len(), 1);
    assert_eq!(resources.resources[0].uri, "workspace://git/ref");
    let templates = pair
        .peer()
        .list_resource_templates(None)
        .await
        .expect("resources/templates/list");
    assert!(templates.resource_templates.is_empty());

    // skills/* → -32601（方法不支持），不是空列表、不是「技能不存在」。
    let failure = pair
        .peer()
        .send_request(rmcp::model::ClientRequest::CustomRequest(
            CustomRequest::new("skills/list", None),
        ))
        .await
        .expect_err("未装配时 skills/list 必须拒绝");
    assert_eq!(mcp_error(failure).code.0, ErrorCode::METHOD_NOT_FOUND.0);

    // 新 scheme 的 read → -32602（未知资源）。
    let failure = pair
        .peer()
        .read_resource(ReadResourceRequestParams::new(
            "skill://project/alpha/SKILL.md",
        ))
        .await
        .expect_err("未装配时技能 URI 必须拒绝");
    assert_eq!(mcp_error(failure).code.0, ErrorCode::INVALID_PARAMS.0);

    pair.shutdown().await;
}

/// 未装配的 provider 类型也可直接构造（对照：内容面在 handler 之外的等价语义）。
#[test]
fn unwired_provider_type_is_construction_only() {
    let cwd = tempdir();
    let provider = WorkspaceResourceProvider::new(cwd.path(), WorkspaceResourcesInput::new());
    // 空输入的 provider 仍是「已装配」：builtin 默认开启（与 handler 的未装配区分）。
    let resources = provider.list_resources();
    assert!(!resources.is_empty());
    let _ = PaginatedRequestParams::default();
}
