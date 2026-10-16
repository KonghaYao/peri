//! builtin 派发 Skill 的**能力预检契约**：Skill 里那张「谁能跑命令」的表必须与
//! shipped Agent 定义逐行一致，靠定义现算维持，不靠关键词断言。
//!
//! 证据结构（正向与负向都可执行）：
//! 1. 事实来源是定义本身：能力表每行由 [`BUILTIN_AGENTS`] 的 frontmatter
//!    （`tools` / `disallowedTools`）现算；
//! 2. Skill 侧只有一份可机读投影（`` | `id` | yes/no | … | ``）；缺行、多行、
//!    取值非法、与定义冲突都判失败；
//! 3. 负例证明判别力：同一 checker 对**篡改过的表**必须报错，不是恒绿断言；
//! 4. 派发试用：`preflight_gaps()` 按「brief 所需工具 ⊆ 目标有效工具」判定，
//!    正例放行、负例在派发前给出缺口名，并证明 `allowedTools` 只能收窄。
//!
//! 边界：本文件只做**声明面**判定，不复制宿主 `filter_tools` /
//! `WorkflowAgentDefinition` 的运行时裁剪；「有效工具集为空」在 subagent 与
//! workflow 两条派发路径上都不报错（现状），所以预检只能由派发方在 brief 里完成。

use super::*;

use peri_acp_types::builtin_mcp::BUILTIN_MCP_INSTANCES;
use peri_acp_types::workspace_resources::{agent_uri, ResourceScope, AGENT_ENTRY_FILE};
use serde_json::Value;

/// 派发 Skill 的注册表名（`BUILTIN_SKILLS` 的键）。
const ULTRACODE: &str = "ultracode";

/// 定义声明的工具边界（与宿主 `filter_tools` 文档口径一致：大小写不敏感；
/// `tools` 省略或独占 `*` = 继承，`tools: []` = 零工具，列表 = 白名单）。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolBoundary {
    Inherit,
    None,
    Allow(Vec<String>),
}

/// 单个目标 Agent 的声明面能力。
struct TargetCapability {
    id: String,
    boundary: ToolBoundary,
    disallowed: Vec<String>,
}

impl TargetCapability {
    /// 声明面是否允许该工具名（精确名、大小写不敏感）。
    fn permits(&self, tool: &str) -> bool {
        let listed = match &self.boundary {
            ToolBoundary::Inherit => true,
            ToolBoundary::None => false,
            ToolBoundary::Allow(list) => {
                list.iter().any(|listed| listed.eq_ignore_ascii_case(tool))
            }
        };
        listed
            && !self
                .disallowed
                .iter()
                .any(|denied| denied.eq_ignore_ascii_case(tool))
    }

    /// 能否运行 OS 命令（`Bash`）。
    fn runs_commands(&self) -> bool {
        self.permits("Bash")
    }
}

/// 派发预检结果：可用的必需工具与缺口。
#[derive(Debug, PartialEq, Eq)]
struct Preflight {
    permitted: Vec<String>,
    gaps: Vec<String>,
}

/// 从定义文本现算声明面能力（builtin 与合成 fixture 走同一条路径）。
fn capability_of(id: &str, content: &str) -> TargetCapability {
    let frontmatter = crate::resources::frontmatter::frontmatter_json(content)
        .unwrap_or_else(|| panic!("{id} 的 frontmatter 必须可解析"));
    TargetCapability {
        id: id.to_string(),
        boundary: tool_boundary(&frontmatter),
        disallowed: string_list(frontmatter.get("disallowedTools")),
    }
}

/// shipped builtin Agent 的声明面能力（事实来源，不复制常量）。
fn builtin_targets() -> Vec<TargetCapability> {
    BUILTIN_AGENTS
        .iter()
        .map(|agent| capability_of(agent.id, agent.content))
        .collect()
}

fn tool_boundary(frontmatter: &serde_json::Map<String, Value>) -> ToolBoundary {
    match frontmatter.get("tools") {
        None | Some(Value::Null) => ToolBoundary::Inherit,
        Some(value) => match string_list(Some(value)).as_slice() {
            [] => ToolBoundary::None,
            [only] if only == "*" => ToolBoundary::Inherit,
            listed => ToolBoundary::Allow(listed.to_vec()),
        },
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(text)) => text
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        Some(other) => panic!("工具声明只能是字符串或数组，实际为 {other}"),
    }
}

/// 取 Skill 正文（frontmatter 已剥离）。
fn skill_body(name: &str) -> &'static str {
    let skill = BUILTIN_SKILLS
        .iter()
        .find(|skill| skill.name == name)
        .unwrap_or_else(|| panic!("builtin 技能 {name} 必须存在"));
    let (_, body) = crate::resources::frontmatter::split_frontmatter(skill.content)
        .unwrap_or_else(|| panic!("{name} 必须有 frontmatter"));
    body
}

/// 解析能力表行（`` | `id` | yes/no | … | ``）；列为原始文本，由 checker 判定。
fn capability_table(body: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 2 {
            continue;
        }
        let Some(id) = cells[0]
            .strip_prefix('`')
            .and_then(|cell| cell.strip_suffix('`'))
        else {
            continue;
        };
        rows.push((id.to_string(), cells[1].to_string()));
    }
    rows
}

/// 能力表 vs 定义：返回全部不一致（空 = 一致）。缺行、多行、取值非法都算。
fn table_mismatches(rows: &[(String, String)], targets: &[TargetCapability]) -> Vec<String> {
    let mut mismatches = Vec::new();
    for (id, claimed) in rows {
        let Some(target) = targets.iter().find(|target| &target.id == id) else {
            mismatches.push(format!("表中有未知目标 `{id}`"));
            continue;
        };
        let expected = if target.runs_commands() { "yes" } else { "no" };
        if claimed != expected {
            mismatches.push(format!(
                "`{id}` 声明 runs commands={claimed}，定义事实为 {expected}"
            ));
        }
        if rows.iter().filter(|(row_id, _)| row_id == id).count() > 1 {
            mismatches.push(format!("`{id}` 在表中出现多次"));
        }
    }
    for target in targets {
        if !rows.iter().any(|(id, _)| id == &target.id) {
            mismatches.push(format!(
                "缺少 `{}` 行（定义事实 runs commands={}）",
                target.id,
                if target.runs_commands() { "yes" } else { "no" }
            ));
        }
    }
    mismatches
}

/// 派发前预检：所需工具必须同时满足定义边界与 workflow `allowedTools` 收窄。
///
/// `allowedTools` 只能收窄——它无法把定义已移除的工具加回来。
fn preflight_gaps(
    target: &TargetCapability,
    required: &[&str],
    allowed_tools: Option<&[&str]>,
) -> Preflight {
    let mut permitted = Vec::new();
    let mut gaps = Vec::new();
    for tool in required {
        let narrowed_out = allowed_tools
            .is_some_and(|allowed| !allowed.iter().any(|item| item.eq_ignore_ascii_case(tool)));
        if target.permits(tool) && !narrowed_out {
            permitted.push((*tool).to_string());
        } else {
            gaps.push((*tool).to_string());
        }
    }
    Preflight { permitted, gaps }
}

fn target<'a>(targets: &'a [TargetCapability], id: &str) -> &'a TargetCapability {
    targets
        .iter()
        .find(|target| target.id == id)
        .unwrap_or_else(|| panic!("builtin 目标 {id} 必须存在"))
}

fn replace_once(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "变异目标必须存在于原文：{from}");
    text.replacen(from, to, 1)
}

#[test]
fn builtin_dispatch_skill_capability_table_matches_definitions() {
    let targets = builtin_targets();
    assert!(!targets.is_empty(), "builtin Agent 注册表不得为空");

    let rows = capability_table(skill_body(ULTRACODE));
    assert!(
        !rows.is_empty(),
        "ultracode 必须给出可机读的能力表（缺表时派发方只能猜目标能力）"
    );
    let mismatches = table_mismatches(&rows, &targets);
    assert!(
        mismatches.is_empty(),
        "ultracode 能力表必须与 shipped 定义一致：{mismatches:#?}"
    );
    assert_eq!(
        rows.len(),
        targets.len(),
        "能力表必须覆盖每个 builtin Agent 且无重复行"
    );
}

#[test]
fn capability_table_check_rejects_tampered_rows() {
    let targets = builtin_targets();
    let body = skill_body(ULTRACODE).to_string();

    // 正向：shipped 表零不一致（与上面的测试同源，确保下面的负例不是恒绿对照）。
    assert!(table_mismatches(&capability_table(&body), &targets).is_empty());

    let mutants = [
        (
            "把 plan 的能力改错",
            replace_once(&body, "| `plan` | yes |", "| `plan` | no |"),
            "`plan`",
        ),
        (
            "少一行目标",
            replace_once(&body, "| `explorer` | yes |", "| explorer | yes |"),
            "explorer",
        ),
        (
            "凭空多一行目标",
            replace_once(
                &body,
                "| `plan` | yes |",
                "| `ghost` | yes | 幽灵目标 |\n| `plan` | yes |",
            ),
            "ghost",
        ),
        (
            "取值非法",
            replace_once(&body, "| `plan` | yes |", "| `plan` | maybe |"),
            "maybe",
        ),
    ];

    for (name, mutant, expected) in mutants {
        let mismatches = table_mismatches(&capability_table(&mutant), &targets);
        assert!(
            !mismatches.is_empty(),
            "负例「{name}」必须被能力表 checker 判失败"
        );
        assert!(
            mismatches.iter().any(|text| text.contains(expected)),
            "负例「{name}」的判定必须指认 {expected}，实际 {mismatches:#?}"
        );
    }
}

#[test]
fn preflight_gaps_expose_missing_tools_before_dispatch() {
    let targets = builtin_targets();

    // 正例：当前 shipped 定义里 plan 继承基础工具，Bash brief 可派发。
    let plan = preflight_gaps(target(&targets, "plan"), &["Bash"], None);
    assert_eq!(plan.gaps, Vec::<String>::new());
    assert!(plan.permitted.contains(&"Bash".to_string()));

    // 负例：显式收窄为空时，预检须发现 Bash 缺口。
    let empty = preflight_gaps(target(&targets, "plan"), &["Bash"], Some(&[]));
    assert_eq!(empty.gaps, vec!["Bash".to_string()]);
    assert!(empty.permitted.is_empty());

    // 正例：只读 brief 派给 plan 是合法路线；提示词约束其任务范围。
    let readonly = preflight_gaps(target(&targets, "plan"), &["Read", "Grep"], None);
    assert_eq!(readonly.gaps, Vec::<String>::new());

    // 负例：allowedTools 只能收窄继承边界。
    let narrowed = preflight_gaps(
        target(&targets, "verification"),
        &["Bash"],
        Some(&["Read", "Grep"]),
    );
    assert_eq!(narrowed.gaps, vec!["Bash".to_string()]);
}

#[test]
fn preflight_reads_frontmatter_not_a_hardcoded_target_list() {
    // 合成 fixture：能力来自定义正文，而不是测试里写死的目标表。
    let no_bash = capability_of(
        "fixture-no-bash",
        "---\nname: fixture-no-bash\ndescription: d\ntools: Read, Grep\n---\nbody\n",
    );
    assert!(!no_bash.runs_commands());
    assert_eq!(
        preflight_gaps(&no_bash, &["Bash"], None).gaps,
        vec!["Bash".to_string()]
    );

    let with_bash = capability_of(
        "fixture-with-bash",
        "---\nname: fixture-with-bash\ndescription: d\ntools: Read, Bash\n---\nbody\n",
    );
    assert!(with_bash.runs_commands());
    assert_eq!(
        preflight_gaps(&with_bash, &["Bash"], None).gaps,
        Vec::<String>::new()
    );

    // 零工具边界与继承边界在声明面必须可分：`tools: []` 不是「继承父工具」。
    let zero_tools = capability_of(
        "fixture-zero",
        "---\nname: fixture-zero\ndescription: d\ntools: []\n---\nbody\n",
    );
    assert!(!zero_tools.runs_commands() && !zero_tools.permits("Read"));
    let inherited = capability_of(
        "fixture-inherit",
        "---\nname: fixture-inherit\ndescription: d\n---\nbody\n",
    );
    assert!(inherited.runs_commands() && inherited.permits("Read"));
}

#[test]
fn preflight_cannot_restore_tools_denied_by_definition() {
    // builtin 当前均继承 Bash，用合成定义保留禁止工具的负向覆盖。
    let denied = capability_of(
        "fixture-denied",
        "---\nname: fixture-denied\ndescription: d\ndisallowedTools: [Bash]\n---\nbody\n",
    );
    let result = preflight_gaps(&denied, &["Bash"], Some(&["Bash"]));
    assert_eq!(result.gaps, vec!["Bash".to_string()]);
    assert!(result.permitted.is_empty());

    // 同一定义仍可满足只读 brief，避免把所有派发都判成失败。
    let readonly = preflight_gaps(&denied, &["Read"], Some(&["Read"]));
    assert!(readonly.gaps.is_empty());
    assert_eq!(readonly.permitted, vec!["Read".to_string()]);
}

#[test]
fn dispatch_guidance_wires_real_tools_and_agent_resources() {
    // 正向：指导文本引用的派发工具名必须是 builtin 注册表里的真实原名
    // （prompt 不得引用旧前缀名或臆造工具）。
    let registry: Vec<&str> = BUILTIN_MCP_INSTANCES
        .iter()
        .flat_map(|instance| instance.tools.iter().map(|tool| tool.original_name))
        .collect();
    let body = skill_body(ULTRACODE);
    for tool in ["Bash", "Read", "Grep"] {
        assert!(
            registry.contains(&tool),
            "{tool} 必须仍是 builtin 注册表的工具名"
        );
        assert!(
            body.contains(&format!("`{tool}`")),
            "能力预检必须点名 {tool}"
        );
    }
    // 负向对照：注册表查找确实能拒绝不存在的工具名（否则上面的断言没有判别力）。
    assert!(
        !registry
            .iter()
            .any(|name| name.eq_ignore_ascii_case("canUseTool")),
        "注册表不得存在臆造的能力 API 工具名"
    );

    // 正向：指导文本给出的 agent 定义读取路径必须是生产 `agent_uri` 的真实输出。
    let plan_uri =
        agent_uri(ResourceScope::Builtin, None, "plan").expect("builtin plan 必须可寻址");
    assert_eq!(plan_uri, format!("agent://builtin/plan/{AGENT_ENTRY_FILE}"));
    assert!(
        body.contains(&plan_uri),
        "能力预检必须给出真实可读的定义 URI：{plan_uri}"
    );
    for target in builtin_targets() {
        assert!(
            agent_uri(ResourceScope::Builtin, None, &target.id).is_some(),
            "builtin 目标 {} 必须可经 agent:// 寻址",
            target.id
        );
    }

    // 负向：指导文本不得让模型调用不存在的能力查询 API。
    for invented in [
        "capabilities(",
        "canUseTool",
        "can_use_tool",
        "capabilityQuery",
    ] {
        assert!(
            !body.contains(invented),
            "能力预检不得引用不存在的 API：{invented}"
        );
    }
}
