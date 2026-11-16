//! 统一 activation 的线路证据（W2）：真实 rmcp 客户端 + 原始 JSON-RPC server
//! （仅 client feature，无 rmcp server 面）。
//!
//! 覆盖验收：激活才读正文（`resources/read` 计数）、digest/frontmatter 失败
//! 不注入、stale 经 `skills/get` 刷新一次后重读、legacy 无绑定条目退化为
//! 发现期已校验正文、断连/取消/非 MCP 来源的显式错误。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use peri_acp_types::mcp_skills::HandleToken;
use peri_acp_types::mcp_skills::McpSkillRegistry;
use peri_acp_types::skills::SkillMetadata;
use peri_acp_types::skills::SkillResource;
use peri_acp_types::skills::SkillSource;
use rmcp::service::RunningService;
use rmcp::RoleClient;
use sha2::{Digest, Sha256};

use super::ActivationError;
use crate::mcp::client::{ClientStatus, McpClientHandle, OAuthStatus};

// ─── 裁决 B（W2 验收）：真实 workspace provider 经生产 client 链路的端到端 ──
//
// 真实 `WorkspaceMcpServer` + W1 `with_resources`（临时 roots，生产装配不动）
// → 生产 transport/client 握手（`spawn_builtin_transport_with_handler` +
// `serve_client_auto`）→ 真实发现（`skills/list`）→ 统一 activation → SkillTool
// 注入文本。生产切换随 W4（不启用宿主投递）。
mod real_workspace_provider {
    use std::path::PathBuf;
    use std::sync::Arc;

    use peri_acp_types::mcp_skills::{HandleToken, McpSkillRegistry};
    use peri_agent::tools::{BaseTool, ToolContext};
    use peri_mcp_workspace::{
        ResourceRoot, ResourceScope, WorkspaceMcpServer, WorkspaceResourcesInput,
    };

    use crate::mcp::builtin::runtime::{BuiltinInstanceSupervisor, BUILTIN_CONVERGE_TIMEOUT};
    use crate::mcp::client::{ClientStatus, McpClientHandle, McpServiceWrapper, OAuthStatus};
    use crate::mcp::McpCapabilityProfile;
    use crate::skills::tools::SkillTool;

    const HANDSHAKE: std::time::Duration = std::time::Duration::from_secs(5);

    /// 真实 provider + 生产链路夹具：临时技能根 + `with_resources` + 生产
    /// transport/client + 真实发现写 registry + 共享同一 registry 的 SkillTool。
    struct RealProvider {
        registry: Arc<McpSkillRegistry>,
        tool: SkillTool,
        entry: PathBuf,
        _root: tempfile::TempDir,
        supervisor: BuiltinInstanceSupervisor,
        service: McpServiceWrapper,
    }

    impl RealProvider {
        async fn wire(body: &str) -> Self {
            let root = tempfile::tempdir().expect("tempdir");
            let dir = root.path().join("alpha");
            std::fs::create_dir_all(&dir).expect("建技能目录");
            let entry = dir.join("SKILL.md");
            std::fs::write(
                &entry,
                format!("---\nname: alpha\ndescription: Alpha skill\n---\n\n{body}\n"),
            )
            .expect("写 SKILL.md");

            let handler = WorkspaceMcpServer::new(root.path().to_string_lossy().to_string(), None)
                .with_resources(
                    WorkspaceResourcesInput::new()
                        .with_skill_root(ResourceRoot::new(root.path(), ResourceScope::Project))
                        .with_disable_bundled(true),
                );
            let transport = crate::mcp::builtin::runtime::spawn_builtin_transport_with_handler(
                "workspace",
                handler,
            );
            let (io, supervisor) = transport.into_parts();
            let service = crate::mcp::client::serve_client_auto(
                io,
                &McpCapabilityProfile::disabled(),
                HANDSHAKE,
            )
            .await
            .expect("生产 client 握手不得超时")
            .expect("生产 client 握手不得失败");

            let handle = Arc::new(McpClientHandle {
                call_gate: Default::default(),
                name: "workspace".to_string(),
                version: None,
                connected_at: None,
                protocol_version: None,
                cache_version: None,
                peer: Some(service.peer().clone()),
                tools: vec![],
                resources: vec![],
                status: ClientStatus::Connected,
                oauth_status: OAuthStatus::default(),
                source: None,
                url: None,
                skills_capable: true,
            });
            let registry = Arc::new(McpSkillRegistry::new());
            let token: HandleToken = handle.clone();
            registry.mark_discovery_started("workspace", token.clone());
            crate::mcp::skill_discovery::run_discovery(
                Arc::clone(&registry),
                None,
                handle,
                token,
                tokio_util::sync::CancellationToken::new(),
            )
            .await;

            let tool = SkillTool::new(Some(Arc::clone(&registry)));
            Self {
                registry,
                tool,
                entry,
                _root: root,
                supervisor,
                service,
            }
        }

        async fn invoke(&self, name: &str) -> Result<String, String> {
            self.tool
                .invoke(
                    serde_json::json!({ "skill_name": name }),
                    ToolContext::new(&[], "/tmp"),
                )
                .await
                .map_err(|error| error.to_string())
        }

        async fn shutdown(mut self) {
            let _ = self
                .service
                .close_with_timeout(std::time::Duration::from_millis(500))
                .await;
            self.supervisor.close(BUILTIN_CONVERGE_TIMEOUT).await;
        }
    }

    /// 裁决 B（W2 验收）：真实 provider 的发现 → activation → 注入文本，
    /// 含 digest 漂移经 `skills/get` 一次刷新重试、内容消失拒绝注入。
    #[tokio::test]
    async fn real_workspace_provider_discovery_activation_and_injection() {
        let fixture = RealProvider::wire("# Alpha v1").await;

        // 1) 发现只发布 metadata：真实 skills/list 已跑过，正文未随条目携带。
        let skills = fixture.registry.all_skills();
        assert_eq!(skills.len(), 1, "真实 provider 应发布一个技能");
        assert_eq!(skills[0].name, "mcp__workspace__alpha");
        assert!(skills[0].content.is_none(), "发现期不得携带正文");
        assert!(skills[0].frontmatter.is_some(), "必须携带 frontmatter 快照");
        assert!(
            skills[0]
                .resources
                .iter()
                .any(|r| r.uri.ends_with("/SKILL.md")),
            "manifest 必须含 SKILL.md 自身条目"
        );

        // 2) 激活 → 注入文本（含来源标注）。
        let out = fixture
            .invoke("mcp__workspace__alpha")
            .await
            .expect("真实 provider 激活应成功");
        assert!(!out.contains("mcp__workspace__"), "不得伪造工具前缀");
        assert!(out.contains("# Alpha v1"), "应注入 v1 正文：{out}");
        assert!(
            out.contains("This skill is served by MCP server \"workspace\""),
            "注入文本必须带来源标注：{out}"
        );

        // 3) digest 漂移：内容被替换 → activation 经 skills/get 刷新一次 →
        //    按新条目重读校验 → 注入新正文（真实 provider 每请求重扫，刷新条目
        //    的 digest 与新内容一致）。
        std::fs::write(
            &fixture.entry,
            "---\nname: alpha\ndescription: Alpha skill\n---\n\n# Alpha v2\n",
        )
        .expect("替换内容");
        let out = fixture
            .invoke("mcp__workspace__alpha")
            .await
            .expect("漂移后经一次刷新应成功");
        assert!(out.contains("# Alpha v2"), "应注入刷新后的 v2 正文：{out}");

        // 4) 内容消失：读取失败 → 显式拒绝（不注入、不回落旧内容）。
        std::fs::remove_file(&fixture.entry).expect("删除 SKILL.md");
        let error = fixture
            .invoke("mcp__workspace__alpha")
            .await
            .expect_err("内容不可得必须显式失败");
        assert!(error.contains("cannot activate"), "实际: {error}");

        fixture.shutdown().await;
    }
}

#[path = "skill_activation_test/test_cases.rs"]
mod test_cases;
