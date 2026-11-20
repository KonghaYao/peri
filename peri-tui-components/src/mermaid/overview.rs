//! 第 4 部分：大图语义概览。
//!
//! 概览只引用原图的有向边，不按节点顺序补连线；完整图与源码始终保留在详情。
//! 分组关系按 `node_to_subgraph()` 的**最内层**归属计算，嵌套组单独列出，
//! 未归组节点使用节点标签作为端点。

use std::collections::{BTreeSet, HashMap, HashSet};

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use mermaid_text::{Graph, parser};

/// 概览一次最多展示的里程碑数量；超过时仍保留首尾并均分取样。
/// 概览默认行预算：对应 40×24 内容区扣除标题后的 21 行。
pub const OVERVIEW_ROW_BUDGET: usize = 21;

const MAX_MILESTONES: usize = 10;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverviewError {
    Unsupported(String),
    Parse(String),
}

impl OverviewError {
    #[must_use]
    pub fn reason(&self) -> &str {
        match self {
            Self::Unsupported(reason) | Self::Parse(reason) => reason,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Relation {
    pub from: Endpoint,
    pub to: Endpoint,
    pub label: Option<String>,
    /// 最内层分组；`None` 表示端点未归组或是直接节点。
    pub from_group: Option<String>,
    pub to_group: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub id: String,
    pub label: String,
    pub depth: usize,
    pub parent: Option<String>,
    /// 直接成员节点（嵌套组内的节点归子组所有）。
    pub node_ids: Vec<String>,
    pub child_ids: Vec<String>,
    pub intra_edges: Vec<Relation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PathPlan {
    path: Vec<String>,
    milestones: Vec<usize>,
    branch_edges: Vec<Relation>,
    /// 来源不在主路径上的边：保持原图声明顺序与重边，逐条补齐结构信息。
    off_path: Vec<Relation>,
    cited_edges: Vec<Relation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Overview {
    pub node_count: usize,
    pub edge_count: usize,
    pub groups: Vec<Group>,
    pub cross_edges: Vec<Relation>,
    pub ungrouped: Vec<Endpoint>,
    ungrouped_edges: Vec<Relation>,
    all_edges: Vec<Relation>,
    labels: HashMap<String, String>,
    group_labels: HashMap<String, String>,
    path: Option<PathPlan>,
}

impl Overview {
    /// 大图才交给概览：有显式分组时按规模判断，无分组时长链优先路径摘要。
    #[must_use]
    pub fn prefers_overview(&self) -> bool {
        if !self.groups.is_empty() {
            self.node_count >= 24 || self.edge_count >= 32 || self.groups.len() >= 8
        } else {
            self.node_count >= 24
        }
    }

    /// 消息区概览：分组图先列跨组关系，无分组长链列路径里程碑。
    pub fn lines(&self, width: usize, max_rows: usize) -> Vec<String> {
        if max_rows == 0 || width == 0 {
            return Vec::new();
        }
        if self.groups.is_empty() {
            self.chain_lines(width, max_rows)
        } else {
            self.grouped_lines(width, max_rows)
        }
    }

    /// 跨组边逐条详情（保持原图声明顺序与重复边）。
    pub fn cross_lines(&self, width: usize) -> Vec<String> {
        self.cross_edges
            .iter()
            .map(|relation| {
                let from_group = relation
                    .from_group
                    .as_deref()
                    .and_then(|id| self.group_labels.get(id))
                    .map_or("未归组", String::as_str);
                let to_group = relation
                    .to_group
                    .as_deref()
                    .and_then(|id| self.group_labels.get(id))
                    .map_or("未归组", String::as_str);
                let mut line = format!(
                    "{} [{from_group}] → {} [{to_group}]",
                    relation.from.label, relation.to.label
                );
                if let Some(label) = relation.label.as_deref() {
                    line.push(' ');
                    line.push_str(label);
                }
                fit(&line, width)
            })
            .collect()
    }

    /// 展开一个分组：原始节点、直接子组与组内全部边。
    pub fn group_detail_lines(&self, group_id: &str, width: usize) -> Vec<String> {
        let Some(group) = self.groups.iter().find(|group| group.id == group_id) else {
            return vec![fit(&format!("找不到分组 {group_id}"), width)];
        };
        let mut lines = Vec::new();
        lines.push(fit(
            &format!(
                "{} · {} 节点 · {} 组内边",
                group.label,
                group.node_ids.len(),
                group.intra_edges.len()
            ),
            width,
        ));
        lines.push(fit(
            &format!("id {} · 深度 {}", group.id, group.depth),
            width,
        ));
        if !group.child_ids.is_empty() {
            let children: Vec<&str> = group
                .child_ids
                .iter()
                .map(|id| {
                    self.group_labels
                        .get(id)
                        .map_or(id.as_str(), String::as_str)
                })
                .collect();
            lines.push(fit(&format!("子组: {}", children.join(" ")), width));
        }
        for node_id in &group.node_ids {
            lines.push(fit(&format!("· {}", self.node_display(node_id)), width));
        }
        for edge in &group.intra_edges {
            lines.push(fit(&self.edge_label_line(edge), width));
        }
        let touching = self
            .cross_edges
            .iter()
            .filter(|edge| {
                edge.from_group.as_deref() == Some(group.id.as_str())
                    || edge.to_group.as_deref() == Some(group.id.as_str())
            })
            .count();
        if touching > 0 {
            lines.push(fit(&format!("跨组边 {touching} 条（见跨组依赖）"), width));
        }
        lines
    }

    #[cfg(test)]
    pub fn summary_edges(&self) -> Vec<Relation> {
        self.path
            .as_ref()
            .map(|path| path.cited_edges.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn edge_exists(&self, from: &str, to: &str) -> bool {
        self.all_edges
            .iter()
            .any(|edge| edge.from.id == from && edge.to.id == to)
    }

    /// 节点行文本：label 与 id 相同或为空时只留 id，避免 `Model Model`。
    fn node_display(&self, node_id: &str) -> String {
        let label = self.labels.get(node_id).map_or(node_id, String::as_str);
        if label.is_empty() || label == node_id {
            node_id.to_string()
        } else {
            format!("{node_id} {label}")
        }
    }

    fn edge_label_line(&self, edge: &Relation) -> String {
        match edge.label.as_deref() {
            Some(label) if !label.is_empty() => {
                format!("{} —{label}→ {}", edge.from.label, edge.to.label)
            }
            _ => format!("{} → {}", edge.from.label, edge.to.label),
        }
    }

    /// 分组概览：跨组关系按首次出现顺序聚合重复项，其余行留作组名与数量。
    fn grouped_lines(&self, width: usize, max_rows: usize) -> Vec<String> {
        let header = format!(
            "{} 组 · {} 节点 · {} 边",
            self.groups.len(),
            self.node_count,
            self.edge_count
        );
        let mut lines = vec![fit(&header, width)];
        if max_rows == 1 {
            return lines;
        }
        let budget = max_rows.saturating_sub(1);
        let tokens = self.aggregated_cross_tokens();
        if tokens.is_empty() {
            // 没有跨组边：先给组名与数量。
            let group_tokens: Vec<String> = self
                .groups
                .iter()
                .map(|group| format!("{}×{}", group.label, group.node_ids.len()))
                .collect();
            for line in pack_tokens(&group_tokens, width)
                .into_iter()
                .take(budget.saturating_sub(1))
            {
                lines.push(line);
            }
            if !self.ungrouped.is_empty() && lines.len() < max_rows {
                lines.push(self.ungrouped_line(width));
            }
            lines.push(self.grouped_hint(width));
            return truncate_lines(lines, max_rows);
        }
        let extra = usize::from(!self.ungrouped.is_empty());
        if tokens.len() + 1 + extra <= budget {
            for token in &tokens {
                lines.push(fit(token, width));
            }
            if !self.ungrouped.is_empty() {
                lines.push(self.ungrouped_line(width));
            }
            lines.push(self.grouped_hint(width));
            return truncate_lines(lines, max_rows);
        }
        if tokens.len() <= budget {
            for token in &tokens {
                lines.push(fit(token, width));
            }
            if !self.ungrouped.is_empty() && lines.len() < max_rows {
                lines.push(self.ungrouped_line(width));
            }
            return truncate_lines(lines, max_rows);
        }

        // 分组太多：先给组名与数量，把逐条关系留给详情。
        let group_tokens: Vec<String> = self
            .groups
            .iter()
            .map(|group| format!("{}×{}", group.label, group.node_ids.len()))
            .collect();
        let packed = pack_tokens(&group_tokens, width);
        let available = budget.saturating_sub(1);
        for line in packed.into_iter().take(available) {
            lines.push(line);
        }
        lines.push(fit(
            &format!("… 另有 {} 条跨组依赖，展开详情查看", tokens.len()),
            width,
        ));
        truncate_lines(lines, max_rows)
    }

    fn grouped_hint(&self, width: usize) -> String {
        let intra: usize = self
            .groups
            .iter()
            .map(|group| group.intra_edges.len())
            .sum();
        fit(&format!("组内边 {intra} 条 · 展开详情"), width)
    }

    fn ungrouped_line(&self, width: usize) -> String {
        let names: Vec<&str> = self
            .ungrouped
            .iter()
            .map(|node| node.label.as_str())
            .collect();
        fit(
            &format!(
                "未归组 {} 节点 · {} 内边: {}",
                self.ungrouped.len(),
                self.ungrouped_edges.len(),
                names.join(" ")
            ),
            width,
        )
    }

    fn aggregated_cross_tokens(&self) -> Vec<String> {
        let mut order: Vec<(String, usize)> = Vec::new();
        for relation in &self.cross_edges {
            let from = self.group_display(&relation.from_group, &relation.from.label);
            let to = self.group_display(&relation.to_group, &relation.to.label);
            let mut token = format!("{from}→{to}");
            if let Some(label) = relation.label.as_deref().filter(|label| !label.is_empty()) {
                token.push(' ');
                token.push_str(label);
            }
            if let Some(existing) = order.iter_mut().find(|(key, _)| *key == token) {
                existing.1 += 1;
            } else {
                order.push((token, 1));
            }
        }
        order
            .into_iter()
            .map(|(token, count)| {
                if count > 1 {
                    format!("{token} ×{count}")
                } else {
                    token
                }
            })
            .collect()
    }

    fn group_display(&self, group: &Option<String>, node_label: &str) -> String {
        group
            .as_deref()
            .and_then(|id| self.group_labels.get(id))
            .cloned()
            .unwrap_or_else(|| node_label.to_string())
    }

    /// 无分组长链（或分组关系过高时的降级）：真实里程碑、分支、首尾与省略数量。
    fn chain_lines(&self, width: usize, max_rows: usize) -> Vec<String> {
        let header = fit(
            &format!("{} 节点 · {} 边", self.node_count, self.edge_count),
            width,
        );
        let mut lines = vec![header];
        if max_rows == 1 {
            return lines;
        }
        let Some(plan) = self.path.as_ref() else {
            lines.push(fit("路径不可用，展开详情查看源码", width));
            return truncate_lines(lines, max_rows);
        };
        let mut body = Vec::new();
        for (position, index) in plan.milestones.iter().enumerate() {
            let node_id = &plan.path[*index];
            body.push(fit(&self.node_display(node_id), width));
            let Some(next_index) = plan.milestones.get(position + 1) else {
                continue;
            };
            let gap = next_index.saturating_sub(*index);
            if gap <= 1 {
                let next = &plan.path[*next_index];
                let edge_label = self
                    .all_edges
                    .iter()
                    .find(|edge| edge.from.id == *node_id && edge.to.id == *next)
                    .and_then(|edge| edge.label.as_deref())
                    .filter(|label| !label.is_empty());
                match edge_label {
                    Some(label) => body.push(fit(&format!("↓ {label}"), width)),
                    None => body.push("↓".to_string()),
                }
            } else {
                let entering = self
                    .all_edges
                    .iter()
                    .find(|edge| {
                        edge.to.id == plan.path[*next_index]
                            && edge.label.as_deref().is_some_and(|label| !label.is_empty())
                    })
                    .and_then(|edge| edge.label.as_deref());
                match entering {
                    Some(label) => body.push(fit(
                        &format!("↓省略{}节点·{label}", gap.saturating_sub(1)),
                        width,
                    )),
                    None => body.push(fit(&format!("↓省略{}节点", gap.saturating_sub(1)), width)),
                }
            }
        }
        for edge in &plan.branch_edges {
            body.push(fit(
                &format!(
                    "↳ {} {}",
                    edge.to.label,
                    edge.label.as_deref().unwrap_or_default()
                ),
                width,
            ));
        }
        for edge in &plan.off_path {
            body.push(fit(&format!("↳ {}", self.edge_label_line(edge)), width));
        }
        let hint = fit(&format!("展开详情查看 {} 节点", self.node_count), width);
        let available = max_rows.saturating_sub(2);
        if body.len() <= available {
            lines.extend(body);
            lines.push(hint);
            return lines;
        }
        let hidden = body.len().saturating_sub(available);
        let mut kept: Vec<String> = body.into_iter().take(available).collect();
        if kept
            .last()
            .is_some_and(|line| line.starts_with('↓') || line.starts_with('↳'))
        {
            kept.pop();
        }
        lines.extend(kept);
        lines.push(fit(&format!("… 另有 {hidden} 条摘要行，展开详情"), width));
        truncate_lines(lines, max_rows)
    }
}

fn truncate_lines(lines: Vec<String>, max_rows: usize) -> Vec<String> {
    lines.into_iter().take(max_rows).collect()
}

fn pack_tokens(tokens: &[String], width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for token in tokens {
        let candidate_width = if current.is_empty() {
            token.width()
        } else {
            current.width() + 1 + token.width()
        };
        if candidate_width <= width {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(token);
        } else {
            if !current.is_empty() {
                lines.push(current);
            }
            current = fit(token, width);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn fit(line: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if line.width() <= width {
        return line.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in line.chars() {
        let cell = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cell > width.saturating_sub(1) {
            break;
        }
        out.push(ch);
        used += cell;
    }
    out.push('…');
    out
}

pub fn build(source: &str) -> Result<Overview, OverviewError> {
    let overview = build_unchecked(source)?;
    if let Err(reason) =
        super::stream::validate_final(source, super::stream::DiagramMode::Flowchart)
    {
        return Err(OverviewError::Parse(one_line(&reason)));
    }
    Ok(overview)
}

/// 流式可信前缀可能还没有真实闭合的围栏，但语句本身已经完整；预览排版
/// 只需要可解析的 Graph，不重复做终稿作用域验收。
pub fn build_unchecked(source: &str) -> Result<Overview, OverviewError> {
    let keyword = source
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("%%"))
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(keyword.as_str(), "graph" | "flowchart") {
        return Err(OverviewError::Unsupported(format!(
            "diagram type '{keyword}' has no semantic overview"
        )));
    }
    let graph = parser::parse(source)
        .map_err(|error| OverviewError::Parse(one_line(&error.to_string())))?;
    Ok(Overview::from_graph(&graph))
}

impl Overview {
    fn from_graph(graph: &Graph) -> Self {
        let labels: HashMap<String, String> = graph
            .nodes
            .iter()
            .map(|node| (node.id.clone(), clean_label(&node.label)))
            .collect();
        let mut groups = Vec::new();
        for subgraph in &graph.subgraphs {
            collect_groups(graph, subgraph, 0, None, &mut groups);
        }
        let group_labels: HashMap<String, String> = groups
            .iter()
            .map(|group| (group.id.clone(), group.label.clone()))
            .collect();
        let node_to_group = graph.node_to_subgraph();

        let all_edges: Vec<Relation> = graph
            .edges
            .iter()
            .map(|edge| {
                let from_group = endpoint_group(&edge.from, &node_to_group, &group_labels);
                let to_group = endpoint_group(&edge.to, &node_to_group, &group_labels);
                Relation {
                    from: endpoint(&edge.from, &labels, &group_labels),
                    to: endpoint(&edge.to, &labels, &group_labels),
                    label: edge.label.as_deref().map(clean_label),
                    from_group,
                    to_group,
                }
            })
            .collect();

        let mut groups_by_id: HashMap<String, usize> = groups
            .iter()
            .enumerate()
            .map(|(index, group)| (group.id.clone(), index))
            .collect();
        let mut cross_edges = Vec::new();
        let mut ungrouped_edges = Vec::new();
        for relation in &all_edges {
            let cross = match (&relation.from_group, &relation.to_group) {
                (Some(from), Some(to)) => from != to,
                (Some(_), None) | (None, Some(_)) => true,
                (None, None) => false,
            };
            if cross {
                cross_edges.push(relation.clone());
            } else if let Some(group) = relation.from_group.as_deref() {
                if let Some(index) = groups_by_id.get_mut(group) {
                    groups[*index].intra_edges.push(relation.clone());
                }
            } else {
                ungrouped_edges.push(relation.clone());
            }
        }

        let ungrouped: Vec<Endpoint> = graph
            .nodes
            .iter()
            .filter(|node| !node_to_group.contains_key(&node.id))
            .map(|node| Endpoint {
                id: node.id.clone(),
                label: node.label.clone(),
            })
            .collect();

        let path = if graph.subgraphs.is_empty() {
            Some(build_path_plan(graph, &all_edges))
        } else {
            None
        };

        Self {
            node_count: graph.nodes.len(),
            edge_count: graph.edges.len(),
            groups,
            cross_edges,
            ungrouped,
            ungrouped_edges,
            all_edges,
            labels,
            group_labels,
            path,
        }
    }
}

fn collect_groups(
    graph: &Graph,
    subgraph: &mermaid_text::types::Subgraph,
    depth: usize,
    parent: Option<&str>,
    out: &mut Vec<Group>,
) {
    if out.iter().any(|group| group.id == subgraph.id) {
        return;
    }
    out.push(Group {
        id: subgraph.id.clone(),
        label: strip_quotes(&subgraph.label),
        depth,
        parent: parent.map(str::to_string),
        node_ids: subgraph.node_ids.clone(),
        child_ids: subgraph.subgraph_ids.clone(),
        intra_edges: Vec::new(),
    });
    for child_id in &subgraph.subgraph_ids {
        if let Some(child) = graph.find_subgraph(child_id) {
            collect_groups(graph, child, depth + 1, Some(&subgraph.id), out);
        }
    }
}

fn strip_quotes(label: &str) -> String {
    let trimmed = label.trim();
    let unquoted = trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .unwrap_or(trimmed);
    clean_label(unquoted)
}

/// 概览是单行字符摘要：节点/边标签里的换行（`<br/>` 解析结果）折成空格。
fn clean_label(label: &str) -> String {
    label
        .replace(['\n', '\r'], " ")
        .trim()
        .trim_matches('"')
        .to_string()
}

fn endpoint_group(
    id: &str,
    node_to_group: &HashMap<String, String>,
    group_labels: &HashMap<String, String>,
) -> Option<String> {
    node_to_group
        .get(id)
        .cloned()
        .or_else(|| group_labels.contains_key(id).then(|| id.to_string()))
}

fn endpoint(
    id: &str,
    labels: &HashMap<String, String>,
    group_labels: &HashMap<String, String>,
) -> Endpoint {
    Endpoint {
        id: id.to_string(),
        label: labels
            .get(id)
            .or_else(|| group_labels.get(id))
            .cloned()
            .unwrap_or_else(|| id.to_string()),
    }
}

fn build_path_plan(graph: &Graph, all_edges: &[Relation]) -> PathPlan {
    let mut outgoing: HashMap<&str, Vec<&Relation>> = HashMap::new();
    let mut indegree: HashMap<&str, usize> = HashMap::new();
    for edge in all_edges {
        outgoing
            .entry(edge.from.id.as_str())
            .or_default()
            .push(edge);
        *indegree.entry(edge.to.id.as_str()).or_default() += 1;
    }
    let Some(mut current) = graph
        .nodes
        .iter()
        .find(|node| !indegree.contains_key(node.id.as_str()))
        .map(|node| node.id.clone())
        .or_else(|| graph.nodes.first().map(|node| node.id.clone()))
    else {
        return PathPlan {
            path: Vec::new(),
            milestones: Vec::new(),
            branch_edges: Vec::new(),
            off_path: Vec::new(),
            cited_edges: Vec::new(),
        };
    };
    let mut path = vec![current.clone()];
    let mut seen: HashSet<String> = path.iter().cloned().collect();
    let mut used_edges: HashSet<(String, String, Option<String>)> = HashSet::new();
    while let Some(edges) = outgoing.get(current.as_str()) {
        let next = edges.iter().find(|edge| !seen.contains(&edge.to.id));
        let Some(edge) = next else {
            break;
        };
        used_edges.insert((edge.from.id.clone(), edge.to.id.clone(), edge.label.clone()));
        current = edge.to.id.clone();
        seen.insert(current.clone());
        path.push(current.clone());
        if path.len() > graph.nodes.len() {
            break;
        }
    }

    // 来源不在主路径上的边（如 Resources→Agent）不属于任何分支行，单独收集。
    let off_path: Vec<Relation> = all_edges
        .iter()
        .filter(|edge| !seen.contains(&edge.from.id))
        .cloned()
        .collect();

    let mut marked: BTreeSet<usize> = BTreeSet::new();
    if !path.is_empty() {
        marked.insert(0);
        marked.insert(path.len() - 1);
    }
    for index in 1..path.len() {
        let has_label = all_edges.iter().any(|edge| {
            edge.from.id == path[index - 1]
                && edge.to.id == path[index]
                && edge.label.as_deref().is_some_and(|label| !label.is_empty())
        });
        if has_label {
            marked.insert(index);
        }
    }
    for (index, node) in path.iter().enumerate() {
        let out_degree = outgoing.get(node.as_str()).map_or(0, Vec::len);
        let in_degree = indegree.get(node.as_str()).copied().unwrap_or(0);
        if out_degree > 1 || in_degree > 1 {
            marked.insert(index);
        }
    }
    let mut milestones: Vec<usize> = marked.into_iter().collect();
    if milestones.len() > MAX_MILESTONES {
        milestones = sample_milestones(&milestones, MAX_MILESTONES);
    }

    let mut branch_edges = Vec::new();
    let mut cited_edges = Vec::new();
    for index in 0..path.len() {
        let node = &path[index];
        let Some(edges) = outgoing.get(node.as_str()) else {
            continue;
        };
        let next_in_path = path.get(index + 1);
        for edge in edges {
            if used_edges.contains(&(edge.from.id.clone(), edge.to.id.clone(), edge.label.clone()))
                && next_in_path == Some(&edge.to.id)
            {
                continue;
            }
            branch_edges.push((*edge).clone());
            cited_edges.push((*edge).clone());
        }
    }
    for pair in path.windows(2) {
        if let Some(edge) = all_edges
            .iter()
            .find(|edge| edge.from.id == pair[0] && edge.to.id == pair[1])
        {
            cited_edges.push((*edge).clone());
        }
    }
    cited_edges.extend(off_path.iter().cloned());
    branch_edges.dedup_by(|a, b| a == b);
    cited_edges.dedup_by(|a, b| a == b);

    PathPlan {
        path,
        milestones,
        branch_edges,
        off_path,
        cited_edges,
    }
}

fn sample_milestones(milestones: &[usize], limit: usize) -> Vec<usize> {
    let mut sampled = Vec::new();
    for slot in 0..limit {
        let index = slot * (milestones.len() - 1) / (limit - 1).max(1);
        sampled.push(milestones[index]);
    }
    sampled.sort_unstable();
    sampled.dedup();
    sampled
}

fn one_line(reason: &str) -> String {
    reason.replace(['\n', '\r'], " ")
}

#[cfg(test)]
#[path = "overview_test.rs"]
mod tests;
