use super::*;

#[tokio::test]
async fn test_tool_filter_inherit_all() {
    // tools is Empty -> inherit all parent tools, but exclude Agent
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Agent"), // this should be excluded
    ];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::Empty;
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(names.contains(&"Write"));
    assert!(!names.contains(&"Agent"), "Agent should not be inherited");
}

#[test]
fn test_tool_filter_allowlist() {
    // tools has value -> only keep specified tools
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Glob")];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::List(vec!["Read".to_string(), "Glob".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(names.contains(&"Glob"));
    assert!(
        !names.contains(&"Write"),
        "Write not in allowlist should be excluded"
    );
}

#[test]
fn test_tool_filter_disallow() {
    // disallowedTools -> exclude from inherited set
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Edit")];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::Empty;
    let disallowed = ToolsValue::List(vec!["Write".to_string(), "Edit".to_string()]);
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(
        !names.contains(&"Write"),
        "Write in disallow list should be excluded"
    );
    assert!(
        !names.contains(&"Edit"),
        "Edit in disallow list should be excluded"
    );
}

#[test]
fn test_tool_filter_wildcard_star() {
    // tools: "*" -> inherit all parent tools (same as Empty), but still exclude Agent
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Bash"),
        make_tool("Agent"), // should still be excluded
    ];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::List(vec!["*".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(
        names.contains(&"Read"),
        "Read should be inherited with tools: *"
    );
    assert!(
        names.contains(&"Write"),
        "Write should be inherited with tools: *"
    );
    assert!(
        names.contains(&"Bash"),
        "Bash should be inherited with tools: *"
    );
    assert!(
        !names.contains(&"Agent"),
        "Agent should still be excluded even with tools: *"
    );
}

#[test]
fn test_tool_filter_wildcard_star_with_disallowed() {
    // tools: "*" + disallowedTools -> inherit all except disallowed
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Write"),
        make_tool("Edit"),
        make_tool("Bash"),
    ];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::List(vec!["*".to_string()]);
    let disallowed = ToolsValue::List(vec!["Write".to_string(), "Edit".to_string()]);
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"), "Read should be inherited");
    assert!(names.contains(&"Bash"), "Bash should be inherited");
    assert!(
        !names.contains(&"Write"),
        "Write in disallow list should be excluded even with tools: *"
    );
    assert!(
        !names.contains(&"Edit"),
        "Edit in disallow list should be excluded even with tools: *"
    );
}

/// Recursion prevention: even if agent.md tools field explicitly includes Agent, it must be excluded
#[test]
fn test_agent_excluded_even_when_explicitly_allowed() {
    let parent_tools = vec![
        make_tool("Read"),
        make_tool("Agent"), // parent tool set has Agent
    ];
    let t = make_subagent_tool(parent_tools);

    // agent.md has tools: ["Agent", "Read"]
    let allowed = ToolsValue::List(vec!["Agent".to_string(), "Read".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"), "Read should be kept");
    assert!(
        !names.contains(&"Agent"),
        "Agent must be excluded even when explicitly in allowlist (recursion prevention)"
    );
}

/// tools/disallowedTools filtering: case-insensitive (users often write PascalCase)
#[test]
fn test_tool_filter_case_insensitive() {
    let parent_tools = vec![make_tool("Read"), make_tool("Write"), make_tool("Glob")];
    let t = make_subagent_tool(parent_tools);

    // User writes different cases in agent.md: tools: READ, glob
    let allowed = ToolsValue::List(vec!["READ".to_string(), "glob".to_string()]);
    let disallowed = ToolsValue::Empty;
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(
        names.contains(&"Read"),
        "Case-insensitive: READ should match Read"
    );
    assert!(
        names.contains(&"Glob"),
        "Case-insensitive: glob should match Glob"
    );
    assert!(
        !names.contains(&"Write"),
        "Write not in allowlist should be excluded"
    );

    // disallowedTools case-insensitive
    let allowed2 = ToolsValue::Empty;
    let disallowed2 = ToolsValue::List(vec!["WRITE".to_string()]);
    let filtered2 = t.filter_tools(&allowed2, &disallowed2);
    let names2: Vec<&str> = filtered2.iter().map(|t| t.name()).collect();

    assert!(names2.contains(&"Read"));
    assert!(names2.contains(&"Glob"));
    assert!(
        !names2.contains(&"Write"),
        "WRITE should case-insensitively exclude Write"
    );
}

/// Recursion prevention: Agent in disallowedTools is redundant but should not error
#[test]
fn test_agent_excluded_when_in_disallowed() {
    let parent_tools = vec![make_tool("Read"), make_tool("Agent")];
    let t = make_subagent_tool(parent_tools);

    let allowed = ToolsValue::Empty;
    let disallowed = ToolsValue::List(vec!["Agent".to_string()]);
    let filtered = t.filter_tools(&allowed, &disallowed);
    let names: Vec<&str> = filtered.iter().map(|t| t.name()).collect();

    assert!(names.contains(&"Read"));
    assert!(!names.contains(&"Agent"), "Agent should not appear");
}
