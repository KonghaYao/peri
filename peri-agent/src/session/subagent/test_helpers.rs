use super::*;
use crate::session::test_resources::mock::admission::FixtureAdmission;

#[path = "delegation_fixture.rs"]
mod delegation_fixture;

pub(super) struct AdmittedSessionFactory;

impl AdmittedSessionFactory {
    pub(super) async fn spawn_subagent(
        parent: Option<&Arc<Session>>,
        mut config: SubagentSpawnConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        config.llm = Box::new(
            crate::session::test_resources::mock::model::PreparedFixtureLlm::new(
                config.llm,
                config.tools.clone(),
            ),
        );
        let resources = config
            .session_resources
            .as_ref()
            .expect("durable subagent fixture")
            .clone();
        let fixture_parent = parent.map(copy_fixture_parent).unwrap_or_else(|| {
            let frozen = FrozenContext::builder()
                .claude_md(config.frozen_claude_md.clone().unwrap_or_default())
                .skill_summary(config.frozen_skill_summary.clone().unwrap_or_default())
                .date(config.frozen_date.clone().unwrap_or_default())
                .build();
            Session::new(
                Arc::from(config.cwd.as_deref().unwrap_or("/tmp")),
                frozen,
                config.parent_thread_id.clone(),
            )
        });
        bind_admission(&fixture_parent, parent, resources);
        let initiator = fixture_parent
            .store()
            .thread_id
            .clone()
            .or_else(|| {
                fixture_parent
                    .subagent_host()
                    .and_then(|host| host.parent_thread_id.clone())
            })
            .unwrap();
        let invocation_id = config
            .parent_tool_call_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        delegation_fixture::prepare_delegation(
            config.session_resources.as_ref().unwrap().as_ref(),
            &initiator,
            &invocation_id,
        )
        .await;
        config.parent_tool_call_id = Some(invocation_id);
        SessionFactory::spawn_subagent(Some(&fixture_parent), config).await
    }

    pub(super) async fn resume_subagent(
        parent: Option<&Arc<Session>>,
        mut config: SubagentResumeConfig,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        config.llm = Box::new(
            crate::session::test_resources::mock::model::PreparedFixtureLlm::new(
                config.llm,
                config.tools.clone(),
            ),
        );
        let fixture_parent = parent.map(copy_fixture_parent).unwrap_or_else(|| {
            Session::new_with_cancel(
                Arc::from(config.cwd.as_deref().unwrap_or("/tmp")),
                FrozenContext::builder()
                    .claude_md(config.frozen_claude_md.clone().unwrap_or_default())
                    .skill_summary(config.frozen_skill_summary.clone().unwrap_or_default())
                    .date(config.frozen_date.clone().unwrap_or_default())
                    .build(),
                None,
                Arc::new(config.cancel_token.clone().unwrap_or_default()),
            )
        });
        bind_admission(&fixture_parent, parent, config.session_resources.clone());
        let initiator = fixture_parent
            .store()
            .thread_id
            .clone()
            .or_else(|| {
                fixture_parent
                    .subagent_host()
                    .and_then(|host| host.parent_thread_id.clone())
            })
            .unwrap_or_else(|| format!("fixture-current-parent:{}", config.thread_id));
        if fixture_parent.store().thread_id.is_none()
            && fixture_parent
                .subagent_host()
                .unwrap()
                .parent_thread_id
                .is_none()
        {
            let resources = config.session_resources.clone();
            let replacement = Session::new_with_cancel(
                fixture_parent.store().cwd.clone(),
                fixture_parent.store().frozen.clone(),
                Some(initiator.clone()),
                fixture_parent.config().cancel_token.clone(),
            );
            bind_admission(&replacement, parent, resources);
            return Self::resume_with_delegation(replacement, config, initiator).await;
        }
        Self::resume_with_delegation(fixture_parent, config, initiator).await
    }

    async fn resume_with_delegation(
        fixture_parent: Arc<Session>,
        mut config: SubagentResumeConfig,
        initiator: String,
    ) -> Result<SubagentSpawned, Box<dyn std::error::Error + Send + Sync>> {
        let invocation_id = config
            .parent_tool_call_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        delegation_fixture::prepare_delegation(
            config.session_resources.as_ref(),
            &initiator,
            &invocation_id,
        )
        .await;
        config.parent_tool_call_id = Some(invocation_id);
        SessionFactory::resume_subagent(Some(&fixture_parent), config).await
    }
}

fn copy_fixture_parent(parent: &Arc<Session>) -> Arc<Session> {
    Session::new_with_cancel(
        parent.store().cwd.clone(),
        parent.store().frozen.clone(),
        parent.store().thread_id.clone(),
        parent.config().cancel_token.clone(),
    )
}

fn bind_admission(
    session: &Arc<Session>,
    parent: Option<&Arc<Session>>,
    resources: Arc<dyn peri_acp_types::session_resources::SessionResources>,
) {
    let mut host = parent
        .and_then(|session| session.subagent_host())
        .as_deref()
        .cloned()
        .unwrap_or_default();
    host.execution_admission_port = Some(Arc::new(FixtureAdmission(resources.clone())));
    host.session_resources = Some(resources);
    session.set_subagent_host(host);
}
