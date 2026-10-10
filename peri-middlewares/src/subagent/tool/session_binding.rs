use std::sync::Arc;

use parking_lot::RwLock;
use peri_agent::middleware::chain::MiddlewareChain;
use peri_agent::session::subagent::{SubagentChainAssembler, SubagentChainContext};
use peri_agent::session::Session;
use peri_agent::tools::BaseTool;

use super::SubAgentTool;

pub(super) struct SessionBoundAssembler {
    template: SubAgentTool,
}

impl SessionBoundAssembler {
    pub(super) fn new(tool: &SubAgentTool) -> Self {
        Self {
            template: tool.clone(),
        }
    }
}

impl SubagentChainAssembler for SessionBoundAssembler {
    fn assemble(&self, context: &SubagentChainContext) -> MiddlewareChain {
        self.template.chain_assembler.assemble(context)
    }

    fn bind_tools(
        &self,
        session: &Arc<Session>,
        tools: Vec<Arc<dyn BaseTool>>,
        tool_filter: peri_agent::session::tool_catalog::ToolFilter,
    ) -> Vec<Arc<dyn BaseTool>> {
        let mut bound = self.template.clone();
        bound.parent_session = Arc::new(RwLock::new(Some(Arc::clone(session))));
        bound.parent_agent_id = Arc::new(RwLock::new(
            session
                .store()
                .thread_id
                .as_deref()
                .map(peri_agent::session::subagent::agent_id_from_child_thread),
        ));
        bound.parent_cwd = session.store().cwd.to_string();
        bound.cancel = Some(session.config().cancel_token.as_ref().clone());
        bound.parent_messages = None;
        bound.inherited_tool_filter = tool_filter;
        bound.parent_tools = Arc::new(tools.clone());
        let replacement: Arc<dyn BaseTool> = Arc::new(bound);
        tools
            .into_iter()
            .map(|tool| {
                if tool.name() == replacement.name() {
                    Arc::clone(&replacement)
                } else {
                    tool
                }
            })
            .collect()
    }
}
