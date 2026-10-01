pub(super) const SCHEMA_OBJECTS_SQL: &str = "SELECT type, name, tbl_name, sql FROM sqlite_master WHERE substr(lower(name), 1, 7) <> 'sqlite_' ORDER BY type, name";
pub(super) const THREAD_COLUMNS_SQL: &str = "SELECT name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo('threads') ORDER BY cid";
pub(super) const MESSAGE_COLUMNS_SQL: &str = "SELECT name, type, \"notnull\", dflt_value, pk, hidden FROM pragma_table_xinfo('messages') ORDER BY cid";
pub(super) const LEGACY_GOALS_SQL: &str = "CREATE TABLE thread_goals (
    thread_id TEXT PRIMARY KEY,
    goal_id TEXT NOT NULL,
    objective TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN
        ('active','paused','blocked','usage_limited','budget_limited','complete')),
    token_budget INTEGER NULL,
    tokens_used INTEGER NOT NULL DEFAULT 0,
    time_used_seconds INTEGER NOT NULL DEFAULT 0,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    FOREIGN KEY(thread_id) REFERENCES threads(id) ON DELETE CASCADE
)";

#[derive(Clone, Debug)]
pub(super) struct SchemaObject {
    pub kind: String,
    pub name: String,
    pub table: String,
    pub sql: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct ColumnShape {
    pub name: String,
    pub kind: String,
    pub not_null: i64,
    pub default: Option<String>,
    pub primary_key: i64,
    pub hidden: i64,
}

pub(super) fn removal_plan(
    objects: &[SchemaObject],
    columns: &[ColumnShape],
) -> Result<Vec<&'static str>, &'static str> {
    let goals = objects
        .iter()
        .find(|object| object.name.eq_ignore_ascii_case("thread_goals"));
    if let Some(object) = goals {
        if object.kind != "table"
            || object.sql.as_deref().map(tokens).transpose()?.as_ref()
                != Some(&tokens(LEGACY_GOALS_SQL)?)
        {
            return Err("unrecognized legacy goal table");
        }
    }
    let mut removed = Vec::new();
    for column in columns {
        let expected = match column.name.to_ascii_lowercase().as_str() {
            "cached_context" => Some(("cached_context", "TEXT", 0, None)),
            "context_cache_epoch" => Some(("context_cache_epoch", "INTEGER", 1, Some("0"))),
            _ => None,
        };
        if let Some((name, kind, not_null, default)) = expected {
            if !column.kind.eq_ignore_ascii_case(kind)
                || column.not_null != not_null
                || column.default.as_deref() != default
                || column.primary_key != 0
                || column.hidden != 0
            {
                return Err("unrecognized legacy cache column");
            }
            removed.push(name);
        }
    }
    for object in objects {
        if object.name.eq_ignore_ascii_case("thread_goals")
            || (object.kind == "index" && object.table.eq_ignore_ascii_case("thread_goals"))
        {
            continue;
        }
        if let Some(sql) = &object.sql {
            let identifiers = tokens(sql)?;
            let references_goals = identifiers.windows(2).any(|pair| {
                pair[0] == "references"
                    && pair[1]
                        .trim_matches('\'')
                        .eq_ignore_ascii_case("thread_goals")
            });
            if identifiers.iter().any(|token| {
                (goals.is_some()
                    && token
                        .trim_matches('\'')
                        .eq_ignore_ascii_case("thread_goals"))
                    || (!object.name.eq_ignore_ascii_case("threads")
                        && removed
                            .iter()
                            .any(|column| token.trim_matches('\'').eq_ignore_ascii_case(column)))
            }) || (goals.is_some() && references_goals)
                || (!removed.is_empty()
                    && object.kind == "view"
                    && identifiers
                        .iter()
                        .any(|token| token.trim_matches('\'').eq_ignore_ascii_case("threads"))
                    && identifiers.iter().any(|token| token == "*"))
            {
                return Err("schema object depends on removed session state");
            }
        }
    }
    let mut plan = Vec::new();
    if goals.is_some() {
        plan.push("DROP TABLE thread_goals");
    }
    if removed.contains(&"cached_context") {
        plan.push("ALTER TABLE threads DROP COLUMN cached_context");
    }
    if removed.contains(&"context_cache_epoch") {
        plan.push("ALTER TABLE threads DROP COLUMN context_cache_epoch");
    }
    Ok(plan)
}

fn tokens(sql: &str) -> Result<Vec<String>, &'static str> {
    let mut chars = sql.chars().peekable();
    let mut result = Vec::new();
    while let Some(character) = chars.next() {
        match character {
            character if character.is_whitespace() || character == ';' => {}
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                for character in chars.by_ref() {
                    if character == '\n' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut closed = false;
                while let Some(character) = chars.next() {
                    if character == '*' && chars.peek() == Some(&'/') {
                        chars.next();
                        closed = true;
                        break;
                    }
                }
                if !closed {
                    return Err("uninterpretable schema definition");
                }
            }
            '\'' | '"' | '`' | '[' => {
                let closing = if character == '[' { ']' } else { character };
                let mut token = String::new();
                let mut closed = false;
                while let Some(next) = chars.next() {
                    if next == closing {
                        if closing != ']' && chars.peek() == Some(&closing) {
                            chars.next();
                            token.push(closing);
                        } else {
                            closed = true;
                            break;
                        }
                    } else {
                        token.push(next);
                    }
                }
                if !closed {
                    return Err("uninterpretable schema definition");
                }
                result.push(if character == '\'' {
                    format!("'{token}'")
                } else {
                    token.to_ascii_lowercase()
                });
            }
            character if character.is_alphanumeric() || character == '_' => {
                let mut token = String::from(character);
                while let Some(next) = chars.peek().copied() {
                    if !next.is_alphanumeric() && next != '_' {
                        break;
                    }
                    token.push(next);
                    chars.next();
                }
                result.push(token.to_ascii_lowercase());
            }
            character => result.push(character.to_string()),
        }
    }
    if result.get(2).map(String::as_str) == Some("if")
        && result.get(3).map(String::as_str) == Some("not")
        && result.get(4).map(String::as_str) == Some("exists")
    {
        result.drain(2..5);
    }
    Ok(result)
}

pub(super) fn known_definition(sql: &str, expected: &str) -> bool {
    matches!((tokens(sql), tokens(expected)), (Ok(actual), Ok(expected)) if actual == expected)
}
