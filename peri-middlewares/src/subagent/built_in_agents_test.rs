use super::*;
use crate::claude_agent_parser::parse_agent_file;

#[test]
fn test_all_built_in_agents_parseable() {
    for agent in list_built_in_agents() {
        let parsed = parse_agent_file(agent.content);
        assert!(
            parsed.is_some(),
            "Built-in agent '{}' failed to parse",
            agent.agent_id
        );
    }
}

#[test]
fn test_built_in_agent_ids_unique() {
    let ids: Vec<&str> = list_built_in_agents().iter().map(|a| a.agent_id).collect();
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(ids, sorted, "Built-in agent IDs should be sorted");
    assert_eq!(
        ids.len(),
        {
            let mut deduped = ids.clone();
            deduped.dedup();
            deduped.len()
        },
        "Built-in agent IDs should be unique"
    );
}

#[test]
fn test_get_built_in_agent_found() {
    assert!(get_built_in_agent("explorer").is_some());
    assert!(get_built_in_agent("plan").is_some());
    assert!(get_built_in_agent("general-purpose").is_some());
    assert!(get_built_in_agent("verification").is_some());
    assert!(get_built_in_agent("web-researcher").is_some());
    assert!(get_built_in_agent("coder").is_some());
}

#[test]
fn test_get_built_in_agent_not_found() {
    assert!(get_built_in_agent("nonexistent").is_none());
    assert!(get_built_in_agent("").is_none());
}

#[test]
fn test_explore_agent_disallows_write_tools() {
    let agent = get_built_in_agent("explorer").unwrap();
    let parsed = parse_agent_file(agent.content).unwrap();
    let disallowed = parsed.disallowed_tools();
    assert!(
        disallowed.iter().any(|t| t.eq_ignore_ascii_case("Write")),
        "Explore agent should disallow Write"
    );
    assert!(
        disallowed.iter().any(|t| t.eq_ignore_ascii_case("Edit")),
        "Explore agent should disallow Edit"
    );
}

#[test]
fn test_general_purpose_has_all_tools() {
    let agent = get_built_in_agent("general-purpose").unwrap();
    let parsed = parse_agent_file(agent.content).unwrap();
    assert!(
        !parsed.tools().is_empty(),
        "General-purpose agent should have tools configured"
    );
}

#[test]
fn test_coder_agent_tools() {
    let agent = get_built_in_agent("coder").unwrap();
    let parsed = parse_agent_file(agent.content).unwrap();
    let tools = parsed.tools();
    assert_eq!(tools.len(), 7, "Coder agent should have exactly 7 tools");
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Edit")),
        "Coder agent should have Edit"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Write")),
        "Coder agent should have Write"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Grep")),
        "Coder agent should have Grep"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Read")),
        "Coder agent should have Read"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Glob")),
        "Coder agent should have Glob"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("Bash")),
        "Coder agent should have Bash"
    );
    assert!(
        tools.iter().any(|t| t.eq_ignore_ascii_case("TodoWrite")),
        "Coder agent should have TodoWrite"
    );
}
