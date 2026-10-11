//! 人机交互契约（自 peri-agent 迁入；`peri-agent::interaction` 保留 re-export）。
//!
//! 统一 HITL（工具审批）与 AskUser（问答）两条路径。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ─── ApprovalItem ──────────────────────────────────────────────────────────────

/// 工具调用审批项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalItem {
    pub tool_call_id: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
}

// ─── QuestionItem ──────────────────────────────────────────────────────────────

/// 问题选项
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub label: String,
    pub description: Option<String>,
}

/// 单个问题
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionItem {
    pub id: String,
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

// ─── InteractionContext ────────────────────────────────────────────────────────

/// 人机交互上下文（描述需要用户响应的场景）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InteractionContext {
    /// 工具调用前审批（原 HITL BatchApprovalRequest）
    Approval { items: Vec<ApprovalItem> },
    /// 向用户提问（原 AskUserBatchRequest）
    Questions { requests: Vec<QuestionItem> },
}

// ─── InteractionResponse ───────────────────────────────────────────────────────

/// 单项审批决策（对齐 HitlDecision 四种语义）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApprovalDecision {
    Approve {
        source: Option<String>,
    },
    Reject {
        reason: String,
        source: Option<String>,
    },
    Edit {
        new_input: serde_json::Value,
    },
    Respond {
        message: String,
    },
}

/// 问题答案
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionAnswer {
    pub id: String,
    pub selected: Vec<String>,
    pub text: Option<String>,
}

/// 交互响应
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum InteractionResponse {
    Decisions(Vec<ApprovalDecision>),
    Answers(Vec<QuestionAnswer>),
    /// 用户明确拒绝了交互（如在 AskUserQuestion 确认弹窗中选择拒绝）
    Rejected,
    /// 无人可以作答：客户端声明自身正常收到提问但无法征询用户（如 `-p` 打印
    /// 模式），与 [`InteractionResponse::Rejected`]（用户拒绝）和空 `Answers`
    /// （伪造成功）都不同，工具须如实转述 `cause` 而不是当作已回答。
    ///
    /// owner 失效、session/prompt 取消、投递失败等生命周期取消不属此变体：它们
    /// 保持既有 cancel 语义，不得伪称客户端声明了原因。
    Unanswered {
        cause: UnansweredCause,
    },
}

// ─── UnansweredCause ───────────────────────────────────────────────────────────

/// elicitation 响应的 `_meta` 键：客户端声明「问题无法被作答」的原因。
///
/// 无交互界面的客户端（如 `-p` 打印模式）在取消 elicitation 时携带此键，
/// broker 据此产出 [`InteractionResponse::Unanswered`]；键缺失保持旧兼容，
/// 键存在但取值无法识别按 [`UnansweredCause::Unknown`] 处理。
pub const ELICITATION_UNANSWERED_META_KEY: &str = "peri.elicitationUnanswered";

/// 客户端无法作答的原因（非用户选择）
///
/// 只承载「无人可作答」这一事实的分类，不承载客户端自由文本：工具据此如实
/// 说明，不转述、不猜测声明之外的内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnansweredCause {
    /// 客户端没有交互界面（如 `-p` 打印模式）：提问已正常送达，但不可能得到用户回答
    NonInteractiveClient,
    /// 客户端声明了「无人可作答」但取值无法识别（更新的客户端或畸形声明）：
    /// 只知道没有用户可以回答，不得声称具体是哪种客户端。
    Unknown,
}

impl UnansweredCause {
    /// 供工具如实转述给模型的说明（不含伪造答案，不指导代替用户授权）。
    pub fn reason_text(self) -> &'static str {
        match self {
            Self::NonInteractiveClient => {
                "客户端是非交互客户端（如 `-p` 打印模式）：提问已正常送达，但该客户端\
                 没有交互界面，没有用户可以作答；这不是用户拒绝或同意，不要把它当作\
                 回答，也不要代替用户完成需要授权的决定"
            }
            Self::Unknown => {
                "客户端声明本次提问无人可作答，但未给出可识别的原因；这不是用户拒绝\
                 或同意，不要把它当作回答，也不要代替用户完成需要授权的决定"
            }
        }
    }

    /// 从 elicitation 响应的 `_meta` 读取客户端声明的原因。
    ///
    /// 只有键（乃至整份 `_meta`）缺失才返回 `None` —— 旧客户端的裸 cancel 语义；
    /// 键存在但取值无法解析时返回 [`UnansweredCause::Unknown`]：声明仍然成立，
    /// 不得当成未声明而伪造空回答。
    ///
    /// # Examples
    ///
    /// ```
    /// use peri_acp_types::interaction::{ELICITATION_UNANSWERED_META_KEY, UnansweredCause};
    /// use serde_json::{Map, Value};
    ///
    /// let mut meta = Map::new();
    /// assert_eq!(UnansweredCause::from_meta(Some(&meta)), None);
    ///
    /// meta.insert(
    ///     ELICITATION_UNANSWERED_META_KEY.to_string(),
    ///     Value::String("non_interactive_client".to_string()),
    /// );
    /// assert_eq!(
    ///     UnansweredCause::from_meta(Some(&meta)),
    ///     Some(UnansweredCause::NonInteractiveClient)
    /// );
    ///
    /// meta.insert(
    ///     ELICITATION_UNANSWERED_META_KEY.to_string(),
    ///     Value::String("some_future_client".to_string()),
    /// );
    /// assert_eq!(
    ///     UnansweredCause::from_meta(Some(&meta)),
    ///     Some(UnansweredCause::Unknown)
    /// );
    /// ```
    pub fn from_meta(meta: Option<&serde_json::Map<String, serde_json::Value>>) -> Option<Self> {
        let value = meta?.get(ELICITATION_UNANSWERED_META_KEY)?;
        Some(Self::deserialize(value).unwrap_or(Self::Unknown))
    }

    /// 构造 `_meta` 条目，供客户端在 elicitation 响应中声明该原因。
    pub fn meta_entry(self) -> (String, serde_json::Value) {
        (
            ELICITATION_UNANSWERED_META_KEY.to_string(),
            serde_json::to_value(self).expect("unit-variant cause must serialize"),
        )
    }
}

// ─── UserInteractionBroker ─────────────────────────────────────────────────────

/// 统一人机交互 broker trait
///
/// 将 HITL（工具审批）和 AskUser（问答）两条路径统一为单一接口。
/// 应用层（TUI / CLI / 测试）实现此 trait，通过 `request` 方法挂起等待用户响应。
///
/// # 使用示例
///
/// ```rust,ignore
/// let broker: Arc<dyn UserInteractionBroker> = Arc::new(TuiInteractionBroker::new(tx));
/// let permission = PermissionMiddleware::with_shared_mode(
///     broker.clone(),
///     default_requires_approval,
///     Arc::new(SharedPermissionMode::new(PermissionMode::Default)),
///     None,
/// );
/// let ask_user_tool = AskUserTool::new(broker);
/// ```
#[async_trait]
pub trait UserInteractionBroker: Send + Sync {
    /// 发起一次人机交互，挂起直到用户响应
    async fn request(&self, ctx: InteractionContext) -> InteractionResponse;
}
