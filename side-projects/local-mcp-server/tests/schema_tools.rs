//! 黄金 schema 冻结测试（WP-001）。
//!
//! 这些测试把 `tests/fixtures/schemas/**` 当作**冻结契约**：夹具本身由
//! `artifacts/test-results/WP-001-r1/extract_tool_schemas.py` 从只读的 Peri 源码逐字
//! 提取（`provenance.source_sha256` 固定源修订），本文件只断言它们满足的形态与数值。
//!
//! 断言分四层：
//! 1. 注册面：恰好七个工具、顺序确定、别名不占用工具条目。
//! 2. 结构：根 `type: object`、`required ⊆ properties`、每个属性有 `type` 与描述、
//!    不使用 `$schema`/`additionalProperties`/组合关键字（保持 2020-12 默认方言基线）。
//! 3. 数值：逐字段 `type`/`minimum`/`maximum`/`default`/`enum` 与源实现一致。
//! 4. 事实登记：宿主投影（Bash 10000 字符外层截断、Read `prefers_persist`）与
//!    别名优先规则必须显式记录，不能被静默丢弃。
//!
//! 消费方：WP-005 的 `tools/list` 必须与本夹具**精确一致**（FC-MCP-01）。

use serde_json::Value;

use local_mcp_server::wire::{resolve_tool_name, TOOL_ALIASES, TOOL_NAMES};

const FIXTURE_SCHEMA: &str = "sandbox-mcp/tool-schema-fixture-v1";

const INDEX_RAW: &str = include_str!("fixtures/schemas/_index.json");
const READ_RAW: &str = include_str!("fixtures/schemas/read.json");
const WRITE_RAW: &str = include_str!("fixtures/schemas/write.json");
const EDIT_RAW: &str = include_str!("fixtures/schemas/edit.json");
const GLOB_RAW: &str = include_str!("fixtures/schemas/glob.json");
const GREP_RAW: &str = include_str!("fixtures/schemas/grep.json");
const FOLDER_RAW: &str = include_str!("fixtures/schemas/folder_operations.json");
const BASH_RAW: &str = include_str!("fixtures/schemas/bash.json");

/// 单字段契约期望。
struct ParamSpec {
    name: &'static str,
    ty: &'static str,
    minimum: Option<i64>,
    maximum: Option<i64>,
    default: Option<bool>,
    enum_values: Option<&'static [&'static str]>,
}

impl ParamSpec {
    const fn plain(name: &'static str, ty: &'static str) -> Self {
        Self {
            name,
            ty,
            minimum: None,
            maximum: None,
            default: None,
            enum_values: None,
        }
    }

    const fn with_minimum(name: &'static str, ty: &'static str, minimum: i64) -> Self {
        Self {
            minimum: Some(minimum),
            ..Self::plain(name, ty)
        }
    }
}

/// 单工具契约期望。
struct ToolSpec {
    tool: &'static str,
    raw: &'static str,
    namespace: &'static str,
    aliases: &'static [&'static str],
    required: &'static [&'static str],
    property_names: &'static [&'static str],
    params: &'static [ParamSpec],
    /// 宿主投影：基类 timeout（毫秒，`None` 表示源实现显式关闭）。
    base_timeout_ms: Option<u64>,
    /// 宿主投影：Peri Agent 层字符上限（`None` 表示未覆写）。
    output_char_limit_chars: Option<u64>,
}

const TOOL_SPECS: &[ToolSpec] = &[
    ToolSpec {
        tool: "Read",
        raw: READ_RAW,
        namespace: "filesystem",
        aliases: &["reading"],
        required: &["file_path"],
        property_names: &["file_path", "offset", "limit", "pages"],
        params: &[
            ParamSpec::plain("file_path", "string"),
            ParamSpec::with_minimum("offset", "integer", 1),
            ParamSpec::with_minimum("limit", "integer", 1),
            ParamSpec::plain("pages", "string"),
        ],
        base_timeout_ms: Some(120_000),
        output_char_limit_chars: None,
    },
    ToolSpec {
        tool: "Write",
        raw: WRITE_RAW,
        namespace: "filesystem",
        aliases: &[],
        required: &["file_path"],
        property_names: &["file_path", "content", "from_draft", "append"],
        params: &[
            ParamSpec::plain("file_path", "string"),
            ParamSpec::plain("content", "string"),
            ParamSpec::plain("from_draft", "string"),
            ParamSpec {
                default: Some(false),
                ..ParamSpec::plain("append", "boolean")
            },
        ],
        base_timeout_ms: None,
        output_char_limit_chars: None,
    },
    ToolSpec {
        tool: "Edit",
        raw: EDIT_RAW,
        namespace: "filesystem",
        aliases: &[],
        required: &["file_path", "old_string", "new_string"],
        property_names: &["file_path", "old_string", "new_string", "replace_all"],
        params: &[
            ParamSpec::plain("file_path", "string"),
            ParamSpec::plain("old_string", "string"),
            ParamSpec::plain("new_string", "string"),
            ParamSpec::plain("replace_all", "boolean"),
        ],
        base_timeout_ms: Some(120_000),
        output_char_limit_chars: None,
    },
    ToolSpec {
        tool: "Glob",
        raw: GLOB_RAW,
        namespace: "filesystem",
        aliases: &[],
        required: &["pattern"],
        property_names: &["pattern", "path"],
        params: &[
            ParamSpec::plain("pattern", "string"),
            ParamSpec::plain("path", "string"),
        ],
        base_timeout_ms: Some(120_000),
        output_char_limit_chars: None,
    },
    ToolSpec {
        tool: "Grep",
        raw: GREP_RAW,
        namespace: "filesystem",
        aliases: &[],
        required: &["pattern"],
        property_names: &[
            "pattern",
            "path",
            "glob",
            "type",
            "output_mode",
            "-i",
            "case_insensitive",
            "-C",
            "context",
            "-A",
            "after_context",
            "-B",
            "before_context",
            "-n",
            "show_line_numbers",
            "multiline",
            "whole_word",
            "invert_match",
            "fixed_strings",
            "max_depth",
            "head_limit",
            "offset",
        ],
        params: &[
            ParamSpec::plain("pattern", "string"),
            ParamSpec::plain("path", "string"),
            ParamSpec::plain("glob", "string"),
            ParamSpec::plain("type", "string"),
            ParamSpec {
                enum_values: Some(&[
                    "content",
                    "files_with_matches",
                    "count",
                    "files_without_matches",
                ]),
                ..ParamSpec::plain("output_mode", "string")
            },
            ParamSpec::plain("-i", "boolean"),
            ParamSpec::plain("case_insensitive", "boolean"),
            ParamSpec::plain("-C", "number"),
            ParamSpec::plain("context", "number"),
            ParamSpec::plain("-A", "number"),
            ParamSpec::plain("after_context", "number"),
            ParamSpec::plain("-B", "number"),
            ParamSpec::plain("before_context", "number"),
            ParamSpec::plain("-n", "boolean"),
            ParamSpec::plain("show_line_numbers", "boolean"),
            ParamSpec::plain("multiline", "boolean"),
            ParamSpec::plain("whole_word", "boolean"),
            ParamSpec::plain("invert_match", "boolean"),
            ParamSpec::plain("fixed_strings", "boolean"),
            ParamSpec::with_minimum("max_depth", "integer", 0),
            ParamSpec::with_minimum("head_limit", "integer", 0),
            ParamSpec::with_minimum("offset", "integer", 0),
        ],
        base_timeout_ms: None,
        output_char_limit_chars: None,
    },
    ToolSpec {
        tool: "folder_operations",
        raw: FOLDER_RAW,
        namespace: "filesystem",
        aliases: &[],
        required: &["operation", "folder_path"],
        property_names: &["operation", "folder_path", "recursive", "max_depth"],
        params: &[
            ParamSpec {
                enum_values: Some(&["create", "list", "exists", "deep_scan"]),
                ..ParamSpec::plain("operation", "string")
            },
            ParamSpec::plain("folder_path", "string"),
            ParamSpec::plain("recursive", "boolean"),
            ParamSpec {
                maximum: Some(10),
                ..ParamSpec::with_minimum("max_depth", "integer", 1)
            },
        ],
        base_timeout_ms: Some(120_000),
        output_char_limit_chars: None,
    },
    ToolSpec {
        tool: "Bash",
        raw: BASH_RAW,
        namespace: "execution",
        aliases: &["Shell"],
        required: &["command"],
        property_names: &["command", "timeout", "run_in_background"],
        params: &[
            ParamSpec::plain("command", "string"),
            ParamSpec::plain("timeout", "number"),
            ParamSpec::plain("run_in_background", "boolean"),
        ],
        base_timeout_ms: None,
        output_char_limit_chars: Some(10_000),
    },
];

/// Grep 语义别名对（语义名，CLI 名），语义名优先。
const GREP_ALIAS_PAIRS: &[(&str, &str)] = &[
    ("case_insensitive", "-i"),
    ("context", "-C"),
    ("after_context", "-A"),
    ("before_context", "-B"),
    ("show_line_numbers", "-n"),
];

fn parse(raw: &str) -> Value {
    serde_json::from_str(raw).expect("夹具必须是合法 JSON")
}

#[test]
fn test_index_declares_exactly_seven_tools_in_frozen_order() {
    let index = parse(INDEX_RAW);
    assert_eq!(index["tool_count"].as_u64(), Some(7));
    let tools: Vec<&str> = index["tools"]
        .as_array()
        .expect("tools 必须是数组")
        .iter()
        .map(|value| value.as_str().expect("工具名必须是字符串"))
        .collect();
    assert_eq!(tools, TOOL_NAMES, "夹具顺序必须等于冻结的 tools/list 顺序");
}

#[test]
fn test_index_alias_table_matches_frozen_aliases() {
    let index = parse(INDEX_RAW);
    let aliases = index["aliases"].as_object().expect("aliases 必须是对象");
    for (alias, canonical) in TOOL_ALIASES {
        let listed = aliases
            .get(canonical)
            .and_then(|value| value.as_array())
            .unwrap_or_else(|| panic!("{canonical} 必须登记别名"));
        assert!(
            listed.iter().any(|value| value.as_str() == Some(alias)),
            "{canonical} 的别名表必须包含 {alias}"
        );
        assert_eq!(resolve_tool_name(alias), Some(canonical));
    }
    assert_eq!(aliases.len(), TOOL_ALIASES.len());
}

#[test]
fn test_every_fixture_uses_the_frozen_shape() {
    for tool_spec in TOOL_SPECS {
        let fixture = parse(tool_spec.raw);
        assert_eq!(fixture["fixture_schema"].as_str(), Some(FIXTURE_SCHEMA));
        assert_eq!(fixture["tool"].as_str(), Some(tool_spec.tool));
        assert_eq!(fixture["namespace"].as_str(), Some(tool_spec.namespace));
        let aliases: Vec<&str> = fixture["aliases"]
            .as_array()
            .unwrap_or_else(|| panic!("{} 缺少 aliases", tool_spec.tool))
            .iter()
            .map(|value| value.as_str().expect("别名必须是字符串"))
            .collect();
        assert_eq!(
            aliases, tool_spec.aliases,
            "{} 的别名表与冻结契约不一致",
            tool_spec.tool
        );
        let description = fixture["description"].as_str().unwrap_or_default();
        assert!(
            !description.trim().is_empty(),
            "{} 缺少工具描述",
            tool_spec.tool
        );
        assert!(
            !fixture["business_error_rule"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .is_empty(),
            "{} 必须登记业务错误规则",
            tool_spec.tool
        );

        let provenance = fixture["provenance"]
            .as_object()
            .unwrap_or_else(|| panic!("{} 缺少 provenance", tool_spec.tool));
        let source_file = provenance["source_file"].as_str().unwrap_or_default();
        assert!(
            source_file.starts_with("peri-middlewares/") && source_file.ends_with(".rs"),
            "{} 的源文件路径必须指向只读 Peri 源: {source_file}",
            tool_spec.tool
        );
        let sha = provenance["source_sha256"].as_str().unwrap_or_default();
        assert_eq!(sha.len(), 64, "{} 的源 sha256 长度异常", tool_spec.tool);
        assert!(
            sha.chars().all(|c| c.is_ascii_hexdigit()),
            "{} 的源 sha256 必须是十六进制",
            tool_spec.tool
        );
    }
}

#[test]
fn test_property_sets_are_frozen() {
    for tool_spec in TOOL_SPECS {
        let fixture = parse(tool_spec.raw);
        let properties = fixture["inputSchema"]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{} 缺少 properties", tool_spec.tool));
        let mut actual: Vec<&str> = properties.keys().map(|key| key.as_str()).collect();
        actual.sort_unstable();
        let mut expected = tool_spec.property_names.to_vec();
        expected.sort_unstable();
        assert_eq!(
            actual, expected,
            "{} 的字段集合与冻结契约不一致",
            tool_spec.tool
        );
        assert_eq!(
            actual.len(),
            tool_spec.params.len(),
            "{} 的字段期望表条数与实际字段数不一致",
            tool_spec.tool
        );
    }
}

#[test]
fn test_required_arrays_are_frozen_and_subset_of_properties() {
    for tool_spec in TOOL_SPECS {
        let fixture = parse(tool_spec.raw);
        let schema = &fixture["inputSchema"];
        let required: Vec<&str> = schema["required"]
            .as_array()
            .unwrap_or_else(|| panic!("{} 缺少 required", tool_spec.tool))
            .iter()
            .map(|value| value.as_str().expect("required 项必须是字符串"))
            .collect();
        assert_eq!(
            required, tool_spec.required,
            "{} 的 required 与冻结契约不一致",
            tool_spec.tool
        );
        let properties = schema["properties"].as_object().expect("properties");
        for name in required {
            assert!(
                properties.contains_key(name),
                "{} 的 required 项 {name} 不在 properties 内",
                tool_spec.tool
            );
        }
    }
}

#[test]
fn test_param_constraints_are_frozen() {
    for tool_spec in TOOL_SPECS {
        let fixture = parse(tool_spec.raw);
        let properties = &fixture["inputSchema"]["properties"];
        for param in tool_spec.params {
            let entry = properties
                .get(param.name)
                .unwrap_or_else(|| panic!("{} 缺少字段 {}", tool_spec.tool, param.name));
            assert_eq!(
                entry["type"].as_str(),
                Some(param.ty),
                "{}.{} 类型不一致",
                tool_spec.tool,
                param.name
            );
            assert_eq!(
                entry.get("minimum").and_then(Value::as_i64),
                param.minimum,
                "{}.{} minimum 不一致",
                tool_spec.tool,
                param.name
            );
            assert_eq!(
                entry.get("maximum").and_then(Value::as_i64),
                param.maximum,
                "{}.{} maximum 不一致",
                tool_spec.tool,
                param.name
            );
            assert_eq!(
                entry.get("default").and_then(Value::as_bool),
                param.default,
                "{}.{} default 不一致",
                tool_spec.tool,
                param.name
            );
            let actual_enum: Option<Vec<&str>> = entry.get("enum").map(|values| {
                values
                    .as_array()
                    .expect("enum 必须是数组")
                    .iter()
                    .map(|value| value.as_str().expect("enum 值必须是字符串"))
                    .collect()
            });
            match param.enum_values {
                Some(expected) => assert_eq!(
                    actual_enum.as_deref(),
                    Some(expected),
                    "{}.{} enum 不一致",
                    tool_spec.tool,
                    param.name
                ),
                None => assert!(
                    actual_enum.is_none(),
                    "{}.{} 不应声明 enum",
                    tool_spec.tool,
                    param.name
                ),
            }
        }
    }
}

#[test]
fn test_root_schema_shape_stays_on_default_2020_12_baseline() {
    for tool_spec in TOOL_SPECS {
        let fixture = parse(tool_spec.raw);
        let schema = &fixture["inputSchema"];
        assert_eq!(
            schema["type"].as_str(),
            Some("object"),
            "{} 的根类型必须是 object",
            tool_spec.tool
        );
        assert!(
            schema.get("$schema").is_none(),
            "{} 不应显式声明方言：省略即默认 JSON Schema 2020-12",
            tool_spec.tool
        );
        for keyword in ["oneOf", "anyOf", "allOf", "$ref", "$defs"] {
            assert!(
                schema.get(keyword).is_none(),
                "{} 的基线 schema 不应使用 {keyword}（保持可校验、无外部引用）",
                tool_spec.tool
            );
        }
        let properties = schema["properties"].as_object().expect("properties");
        for (name, entry) in properties {
            assert!(
                entry.get("type").is_some(),
                "{}.{name} 必须声明 type",
                tool_spec.tool
            );
            assert!(
                !entry["description"].as_str().unwrap_or_default().is_empty(),
                "{}.{name} 必须带非空 description",
                tool_spec.tool
            );
        }
    }
}

#[test]
fn test_tool_names_satisfy_spec_name_rules() {
    for name in TOOL_NAMES {
        assert!((1..=128).contains(&name.len()), "{name} 长度违反规范");
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')),
            "{name} 含规范不允许的字符"
        );
    }
    assert_eq!(TOOL_NAMES.len(), 7);
}

#[test]
fn test_grep_alias_pairs_and_priority_fact_are_recorded() {
    let fixture = parse(GREP_RAW);
    let properties = fixture["inputSchema"]["properties"]
        .as_object()
        .expect("properties");

    let groups = fixture["param_alias_groups"]
        .as_array()
        .expect("Grep 必须登记别名对");
    assert_eq!(groups.len(), GREP_ALIAS_PAIRS.len());
    for (index, (semantic, cli)) in GREP_ALIAS_PAIRS.iter().enumerate() {
        let pair = groups[index].as_array().expect("别名对必须是数组");
        assert_eq!(pair[0].as_str(), Some(*semantic));
        assert_eq!(pair[1].as_str(), Some(*cli));
        assert!(
            properties.contains_key(*semantic) && properties.contains_key(*cli),
            "别名对 {semantic}/{cli} 必须同时存在于 schema"
        );
    }

    let rule = fixture["param_alias_rule"].as_str().unwrap_or_default();
    assert!(rule.contains("语义名优先"), "必须明文登记语义别名优先规则");
}

#[test]
fn test_host_projection_facts_are_registered() {
    for tool_spec in TOOL_SPECS {
        let fixture = parse(tool_spec.raw);
        let projection = &fixture["host_projection"];
        assert_eq!(
            projection["base_tool_timeout_ms"].as_u64(),
            tool_spec.base_timeout_ms,
            "{} 的基类 timeout 事实不一致",
            tool_spec.tool
        );
        assert_eq!(
            projection["output_char_limit_chars"].as_u64(),
            tool_spec.output_char_limit_chars,
            "{} 的外层字符上限事实不一致",
            tool_spec.tool
        );
        assert!(
            !projection["note"].as_str().unwrap_or_default().is_empty(),
            "{} 的宿主投影必须附说明",
            tool_spec.tool
        );
    }

    // 两个最容易"静默丢失"的事实：Bash 的 10000 字符外层投影、Read 的 prefers_persist。
    let bash = parse(BASH_RAW);
    assert_eq!(
        bash["host_projection"]["output_char_limit_chars"].as_u64(),
        Some(10_000)
    );
    let read = parse(READ_RAW);
    assert_eq!(
        read["host_projection"]["prefers_persist"].as_bool(),
        Some(true)
    );
}

#[test]
fn test_required_error_messages_are_recorded_per_tool() {
    for tool_spec in TOOL_SPECS {
        let fixture = parse(tool_spec.raw);
        let messages = fixture["required_error_messages"]
            .as_object()
            .unwrap_or_else(|| panic!("{} 必须登记必需参数错误文案", tool_spec.tool));
        assert!(
            !messages.is_empty(),
            "{} 的必需参数错误文案不能为空",
            tool_spec.tool
        );
        for name in tool_spec.required {
            // Edit 的三个必需参数、folder 的两个必需参数都必须逐条登记。
            if tool_spec.tool == "Write" {
                continue; // Write 的 file_path 必填，content/from_draft 是二选一
            }
            assert!(
                messages.contains_key(*name),
                "{} 缺少必需参数 {name} 的错误文案",
                tool_spec.tool
            );
        }
    }
}

#[test]
fn test_fixture_truncation_and_limit_facts_are_present_in_descriptions() {
    // 截断语义是迁移对等性的核心，描述文本必须保留源实现的上限说明。
    let cases: &[(&str, &[&str])] = &[
        (READ_RAW, &["2000", "65536", "5000", "32 MB"]),
        (GLOB_RAW, &["1000", "20000"]),
        (GREP_RAW, &["1000", "20000", "250"]),
        (FOLDER_RAW, &["500", "max_depth"]),
        (BASH_RAW, &["65000", "2000", "600000", "15000"]),
    ];
    for (raw, needles) in cases {
        let fixture = parse(raw);
        let description = fixture["description"].as_str().unwrap_or_default();
        for needle in *needles {
            assert!(
                description.contains(needle),
                "{} 的描述必须保留上限事实 {needle}",
                fixture["tool"].as_str().unwrap_or("?")
            );
        }
    }
}
