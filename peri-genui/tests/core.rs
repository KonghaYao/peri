use std::sync::Arc;

use peri_genui::{
    evaluate_root, parse, scan_blocks, system_prompt, ErrorCode, EvalContext, EvalError, Library,
    PromptOptions, StatementKind, StreamParser,
};
use serde_json::{json, Value};

fn library() -> Library {
    Library::from_json_str(include_str!("library.json")).unwrap()
}

#[test]
fn schema_argument_order_is_not_alphabetical() {
    let lib = Library::from_json_str(r#"{"root":"Widget","schema":{"properties":{"Widget":{}},"$defs":{"Widget":{"properties":{"z":{"type":"string"},"a":{"type":"number"}},"required":["z"]}}}}"#).unwrap();
    assert_eq!(lib.root(), Some("Widget"));
    let result = parse("root = Widget(\"first\", 7)", &lib);
    let root = result.root.unwrap();
    assert_eq!(
        root.props.keys().map(String::as_str).collect::<Vec<_>>(),
        ["z", "a"]
    );
    assert_eq!(root.props["z"], "first");
    assert_eq!(root.props["a"], 7);
}

#[test]
fn bare_schema_and_explicit_root_are_supported() {
    let schema = json!({"properties":{"Widget":{}}, "$defs":{"Widget":{"properties":{"value":{"type":"number"}}}}});
    let lib = Library::from_value(schema, Some("Widget".into())).unwrap();
    assert_eq!(lib.root(), Some("Widget"));
    assert_eq!(
        parse("root = Widget(2)", &lib).to_fixture_json()["root"]["props"]["value"],
        2
    );
    assert!(Library::from_value(json!({}), None).is_err());
    assert!(Library::from_json_str("{").is_err());
}

#[test]
fn library_rejects_missing_root_collisions_and_action_cycles() {
    for input in [
        r#"{"root":"Missing","schema":{"$defs":{"Text":{"properties":{}}}}}"#,
        r#"{"$defs":{"Count":{"properties":{}}}}"#,
        r#"{"$defs":{"Text":{"properties":{}}},"functions":{"Text":{"params":{}}}}"#,
        r##"{"$defs":{"Text":{"properties":{"action":{"$ref":"#/$defs/ActionExpression"}}},"ActionExpression":{"$ref":"#/$defs/ActionExpression"}}}"##,
    ] {
        assert!(Library::from_json_str(input).is_err(), "{input}");
    }
}

#[test]
fn action_schema_expansion_is_bounded() {
    let mut defs = serde_json::Map::new();
    defs.insert(
        "Text".into(),
        json!({"properties": {"x": {"$ref": "#/$defs/A0"}}}),
    );
    let mut actions = serde_json::Map::new();
    for i in 0..25 {
        let next = i + 1;
        let def = if i == 24 {
            json!({"type": "object"})
        } else {
            json!({"anyOf": [
                {"$ref": format!("#/$defs/A{next}")},
                {"$ref": format!("#/$defs/A{next}")}
            ]})
        };
        defs.insert(format!("A{i}"), def);
        actions.insert(format!("A{i}"), json!({"params": {"type": "object"}}));
    }
    let document = json!({
        "root": "Text",
        "schema": {
            "properties": {"Text": {}},
            "$defs": defs,
            "actions": actions
        }
    });
    let error = Library::from_json_str(&document.to_string()).unwrap_err();
    assert!(error.to_string().contains("预算"), "{error}");
}

#[test]
fn state_reacts_without_mutating_parse_result() {
    let lib = library();
    let result = parse(
        "root = Stack([Input(\"q\", null, $q), Text($q)])\n$q = \"before\"",
        &lib,
    );
    let mut context = EvalContext::default();
    context.state.insert("$q".into(), json!("after"));
    let actual = result.to_fixture_json_with_context(&context);
    assert_eq!(
        actual["root"]["props"]["children"][0]["props"]["value"],
        json!({"$binding":"$q"})
    );
    assert_eq!(
        actual["root"]["props"]["children"][1]["props"]["text"],
        "after"
    );
    assert_eq!(result.state["$q"], "before");
}

#[test]
fn ternary_keeps_its_else_after_a_trailing_colon_newline() {
    let lib = library();
    for (show, expected) in [(false, "no"), (true, "yes")] {
        let source = format!("root = $show ? Text(\"yes\") :\nText(\"no\")\n$show = {show}");
        let actual = parse(&source, &lib).to_fixture_json();
        assert_eq!(actual["errors"], json!([]));
        assert_eq!(actual["root"]["props"]["text"], expected);
    }
    for (show, expected) in [(false, "no"), (true, "yes")] {
        let source = format!("root = $show\n  ? Text(\"yes\")\n  : Text(\"no\")\n$show = {show}");
        let actual = parse(&source, &lib).to_fixture_json();
        assert_eq!(actual["errors"], json!([]));
        assert_eq!(actual["root"]["props"]["text"], expected);
    }
}

#[test]
fn state_defaults_follow_declaration_order_and_only_use_builtin_functions() {
    let lib = library();
    let result = parse("root = Stack([])\n$a = 2\n$b = $a + 1\n$c = @Sum([1, 2])\n$d = @ToAssistant(\"no\")\n$e = @Upper(\"no\")\n$f = $later\n$later = 9\n$g = {x: $b, y: @Count([0])}", &lib);
    assert_eq!(result.state["$b"], 3);
    assert_eq!(result.state["$c"], 3);
    assert_eq!(result.state["$d"], Value::Null);
    assert_eq!(result.state["$e"], Value::Null);
    assert_eq!(result.state["$f"], Value::Null);
    assert_eq!(result.state["$g"], json!({"x":3,"y":1}));
}

#[test]
fn all_builtin_functions_and_elementwise_numbers() {
    let lib = library();
    let result = parse(
        r#"root = Stack([Table([Col("values", [@Count([1,2]), @First([3,4]), @Last([3,4]), @Sum([1,"2",true]), @Avg([2,4]), @Min([2,4]), @Max([2,4]), @Round([1.25,[-1.5]],1), @Abs([-2,[-3]]), @Floor([1.9,-1.1]), @Ceil([1.1,-1.9]), @Filter([{n:1},{n:2}],"n",">",1), @Sort([10,2,1],"")])])])"#,
        &lib,
    );
    let actual = result.to_fixture_json();
    assert_eq!(actual["errors"], json!([]));
    assert_eq!(
        actual["root"]["props"]["children"][0]["props"]["columns"][0]["props"]["data"],
        json!([2,3,4,4,3,2,4,[1.3,[-1.5]],[2,[3]],[1,-2],[2,-1],[{"n":2}],[1,2,10]])
    );
}

#[test]
fn each_captures_loop_values_in_deferred_set_actions() {
    let lib = library();
    let result = parse(
        r#"root = Stack(@Each([{id:1},{id:2}], "item", Button("pick", @Set($chosen, item.id))))"#,
        &lib,
    );
    let actual = result.to_fixture_json();
    assert_eq!(actual["errors"], json!([]));
    assert_eq!(actual["unresolved"], json!([]));
    let children = &actual["root"]["props"]["children"];
    assert_eq!(
        children[0]["props"]["action"]["actions"][0]["args"],
        json!(["$chosen", 1])
    );
    assert_eq!(
        children[1]["props"]["action"]["actions"][0]["args"],
        json!(["$chosen", 2])
    );
}

#[test]
fn each_binds_loop_variables_only_inside_its_template() {
    let lib = library();
    for inner_var in ["\"item\"", "item"] {
        let nested = parse(
            &format!(
                r#"root = Stack(@Each([{{name:"outer"}}], "item", Stack(@Each([{{name:"inner"}}], {inner_var}, Text(item.name)))))"#
            ),
            &lib,
        );
        let actual = nested.to_fixture_json();
        assert_eq!(actual["errors"], json!([]));
        assert_eq!(
            actual["root"]["props"]["children"][0]["props"]["children"][0]["props"]["text"],
            "inner"
        );
    }
    let same_name = parse(
        r#"items = [{name:"a"}, {name:"b"}]
root = Stack(@Each(items, "items", Text(items.name)))"#,
        &lib,
    );
    let actual = same_name.to_fixture_json();
    assert_eq!(actual["errors"], json!([]));
    assert_eq!(
        actual["root"]["props"]["children"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(actual["root"]["props"]["children"][0]["props"]["text"], "a");
}

#[test]
fn evaluated_props_are_validated_and_defaults_applied() {
    let lib = library();
    let actual = parse("root = Text($text)\n$text = 7", &lib).to_fixture_json();
    assert!(actual["root"].is_null());
    assert!(actual["errorObjects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| {
            error["code"] == "type-mismatch"
                && error["statementId"] == "root"
                && error["component"] == "Text"
                && error["path"] == "/text"
        }));

    let actual = parse(r#"root = Stack(@Each([1], "item", Text()))"#, &lib).to_fixture_json();
    assert!(actual["errorObjects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|error| {
            error["code"] == "missing-required"
                && error["component"] == "Text"
                && error["path"] == "/text"
        }));

    let actual = parse("root = Text($text)\n$text = \"hi\"", &lib).to_fixture_json();
    assert_eq!(actual["errors"], json!([]));
    assert_eq!(actual["root"]["props"]["text"], "hi");

    let lib = Library::from_json_str(r#"{"$defs":{"Widget":{"properties":{"config":{"type":"object","properties":{"x":{"type":"number"}},"required":["x"]},"title":{"type":"string","default":"fallback"}},"required":["config","title"]}}}"#).unwrap();
    let actual = parse("root = Widget({x:1}, $title)\n$title = null", &lib).to_fixture_json();
    assert_eq!(actual["errors"], json!([]));
    assert_eq!(actual["root"]["props"]["title"], "fallback");
}

#[test]
fn query_and_mutation_parse_but_evaluation_is_explicitly_unsupported() {
    let lib = library();
    let result = parse(
        r#"root = Stack([Button("go", Action([@Run(q), @Run(m)])), Text(q.name)])
q = Query("read", {}, {name:"default"})
m = Mutation("write", {})"#,
        &lib,
    );
    assert!(result.errors.is_empty());
    assert_eq!(result.statements["q"].kind, StatementKind::Query);
    assert_eq!(result.statements["m"].kind, StatementKind::Mutation);
    let evaluated = evaluate_root(&result, &EvalContext::default());
    for id in ["q", "m"] {
        assert!(evaluated.errors.iter().any(
            |e| matches!(e, EvalError::Unsupported { statement_id, .. } if statement_id == id)
        ));
    }
    let root = result.to_fixture_json()["root"].clone();
    assert_eq!(
        root["props"]["children"][0]["props"]["action"]["actions"],
        json!([{"action":"Run","args":["q"]},{"action":"Run","args":["m"]}])
    );
}

#[test]
fn query_with_dollar_name_is_not_a_state_declaration() {
    let lib = library();
    let result = parse("$q = Query(\"read\", {}, {})\nroot = Stack([])", &lib);
    assert_eq!(result.statements["$q"].kind, StatementKind::Query);
    assert!(!result.state.contains_key("$q"));
    assert!(matches!(
        &evaluate_root(&result, &EvalContext::default()).errors[0],
        EvalError::Unsupported { .. }
    ));
}

#[test]
fn inline_reserved_calls_drop_only_that_node() {
    let lib = library();
    let result = parse(
        r#"root = Stack([Mutation("write", {}), Query("read", {}), Text("keep")])"#,
        &lib,
    );
    assert_eq!(
        result
            .errors
            .iter()
            .filter(|e| e.code == ErrorCode::InlineReserved)
            .count(),
        2
    );
    assert_eq!(
        result.to_fixture_json()["root"]["props"]["children"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn newline_inside_parentheses_keeps_binary_minus() {
    let lib = library();
    let actual = parse("root = Metric(\"n\", (4\n-1))", &lib).to_fixture_json();
    assert_eq!(actual["errors"], json!([]));
    assert_eq!(actual["root"]["props"]["value"], 3);

    let actual = parse("root = Stack([Col(\"c\", [1 -2, 3, 4 - 1,\n-5])])", &lib).to_fixture_json();
    assert_eq!(actual["errors"], json!([]));
    assert_eq!(
        actual["root"]["props"]["children"][0]["props"]["data"],
        json!([-1, 3, 3, -5])
    );
}

#[test]
fn validation_prunes_nested_data_and_uses_schema_defaults() {
    let lib = Library::from_json_str(r#"{"$defs":{"Widget":{"properties":{"config":{"type":"object","properties":{"x":{"type":"number","default":5},"y":{"type":"string"},"items":{"type":"array","items":{"type":"number"}}},"required":["x"]},"title":{"type":"string","default":"fallback"}},"required":["config","title"]}}}"#).unwrap();
    let result = parse(
        r#"root = Widget({x:"bad", y:3, items:[1,"bad",2]}, null)"#,
        &lib,
    );
    let actual = result.to_fixture_json();
    assert_eq!(
        actual["root"]["props"],
        json!({"config":{"x":5,"items":[1,2]},"title":"fallback"})
    );
    let paths = result
        .errors
        .iter()
        .map(|e| e.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["/config/x", "/config/y", "/config/items/1"]);
}

#[test]
fn typed_slots_reject_wrong_components_and_structured_enum_values() {
    let lib = library();
    let result = parse("root = Table([Text(\"wrong\")])", &lib);
    assert!(result
        .errors
        .iter()
        .any(|error| { error.code == ErrorCode::TypeMismatch && error.path == "/columns/0" }));
    assert!(result.errors.iter().any(|error| {
        error.code == ErrorCode::TypeMismatch && error.message.contains("component Text")
    }));

    let result = parse("root = Table([Col(\"ok\")])", &lib);
    assert_eq!(result.errors, vec![]);

    for value in ["{}", "[]"] {
        let result = parse(&format!("root = Stack([Tag(\"x\", {value})])"), &lib);
        assert!(result
            .errors
            .iter()
            .any(|error| { error.code == ErrorCode::TypeMismatch && error.path == "/color" }));
    }
    let result = parse("root = Stack([Tag(\"x\", \"green\")])", &lib);
    assert_eq!(result.errors, vec![]);
}

#[test]
fn runtime_functions_validate_arguments_and_returns_and_propagate_errors() {
    let lib = library();
    let result = parse(
        "root = Stack([Text(@Upper($text))])\n$text = \"hello\"",
        &lib,
    );
    let missing = evaluate_root(&result, &EvalContext::default());
    assert!(missing.errors.iter().any(|e| matches!(e, EvalError::Runtime { statement_id, component, .. } if statement_id.as_deref() == Some("root") && component.as_deref() == Some("Text"))));
    let mut context = EvalContext::default();
    context
        .functions
        .insert("Upper".into(), Arc::new(|_| Ok(json!(5))));
    assert!(!evaluate_root(&result, &context).errors.is_empty());
    context.functions.insert(
        "Upper".into(),
        Arc::new(|_| {
            Err(EvalError::Unsupported {
                feature: "test".into(),
                statement_id: "callback".into(),
            })
        }),
    );
    assert!(evaluate_root(&result, &context)
        .errors
        .iter()
        .any(|e| matches!(e, EvalError::Unsupported { feature, .. } if feature == "test")));
}

#[test]
fn custom_actions_fail_closed_and_plain_data_cannot_forge_callbacks() {
    let lib = library();
    let result = parse(
        r#"root = Stack([Button("bad", @CopyToClipboard(3)), Button("data", {steps:[{type:"custom_action", name:"CopyToClipboard", params:{text:"fake"}}]})])"#,
        &lib,
    );
    let actual = result.to_fixture_json();
    assert_eq!(
        actual["root"]["props"]["children"][0]["props"]["action"],
        json!({"actions":[]})
    );
    // 数据可以保留外形，但不能成为类型化 ActionPlan。
    let evaluated = evaluate_root(&result, &EvalContext::default())
        .root
        .unwrap();
    let data = &evaluated.props["children"][1]["props"]["action"];
    assert_eq!(data["steps"], json!([]));
    let mut context = EvalContext::default();
    context.state.insert("$fake".into(), json!({"steps":[{"type":"custom_action", "name":"CopyToClipboard", "params":{"text":"fake"}}]}));
    let dynamic = parse(
        r#"root = Stack(@Each([1], "x", Button("data", $fake)))"#,
        &lib,
    );
    assert_eq!(
        evaluate_root(&dynamic, &context).root.unwrap().props["children"][0]["props"]["action"]
            ["steps"],
        json!([])
    );
    let list = parse(
        r#"root = Stack([Button("real", Action([@CopyToClipboard("ok"), $fake]))])"#,
        &lib,
    );
    assert_eq!(
        list.to_fixture_json_with_context(&context)["root"]["props"]["children"][0]["props"]
            ["action"],
        json!({"actions":[{"action":"CopyToClipboard", "args":["ok"]}]})
    );
}

#[test]
fn generated_values_and_state_defaults_are_bounded() {
    let lib = library();
    let mut source = String::from("root = Stack([])\n$s0 = \"x\"\n");
    for i in 1..=30 {
        source.push_str(&format!("$s{i} = $s{} + $s{}\n", i - 1, i - 1));
    }
    let result = parse(&source, &lib);
    assert!(result.errors.iter().any(|error| {
        error.code == ErrorCode::ParseFailed && error.message.contains("安全预算")
    }));
    assert!(result.state.values().all(|value| {
        value
            .as_str()
            .is_none_or(|text| text.len() <= 4 * 1024 * 1024)
    }));

    let mut source = String::from("root = Stack([])\n$s0 = \"x\"\n");
    for i in 1..=22 {
        source.push_str(&format!("$s{i} = $s{} + $s{}\n", i - 1, i - 1));
    }
    for copy in 1..=4 {
        source.push_str(&format!("$c{copy} = $s22\n"));
    }
    let result = parse(&source, &lib);
    assert!(result.errors.iter().any(|error| {
        error.code == ErrorCode::ParseFailed && error.message.contains("state 默认值超过安全预算")
    }));
    assert_eq!(
        result.state["$c2"].as_str().map(str::len),
        Some(4 * 1024 * 1024)
    );
    assert!(result.state["$c3"].is_null());

    let mut source =
        String::from("root = Stack([Text($s22 + $s22), Col(\"c\", [$s22, $s22])])\n$s0 = \"x\"\n");
    for i in 1..=22 {
        source.push_str(&format!("$s{i} = $s{} + $s{}\n", i - 1, i - 1));
    }
    let result = parse(&source, &lib);
    assert_eq!(result.errors, vec![]);
    let evaluated = evaluate_root(&result, &EvalContext::default());
    assert!(evaluated
        .errors
        .iter()
        .any(|error| matches!(error, EvalError::LimitExceeded)));
}

#[test]
fn stream_replacement_finish_and_incomplete_state_tail() {
    let lib = Arc::new(library());
    let mut stream = StreamParser::new(lib.clone());
    let pending = stream.push("root = Stack([Text($na");
    assert!(pending.incomplete);
    assert!(!pending.state.contains_key("$na"));
    stream.push("me)])\n$name = \"done\"");
    assert_eq!(
        stream.finish().to_fixture_json(),
        parse("root = Stack([Text($name)])\n$name = \"done\"", &lib).to_fixture_json()
    );
    let replaced = stream.set("root = Card(\"new\")", true);
    assert_eq!(replaced.to_fixture_json()["root"]["props"]["title"], "new");
    assert_eq!(
        stream.finish().to_fixture_json(),
        replaced.to_fixture_json()
    );
}

#[test]
fn scanner_preserves_utf8_ranges_crlf_and_opener_code() {
    let input =
        "前言\r\n```gen-ui root = Card(\"中文\")\r\n```\r\n尾声\n~~~gen-ui\nroot = Stack([])\n";
    let blocks = scan_blocks(input);
    assert_eq!(blocks.len(), 2);
    for block in &blocks {
        assert_eq!(&input[block.raw_range.clone()], block.raw);
        assert_eq!(&input[block.source_range.clone()], block.source);
    }
    assert!(blocks[0].closed);
    assert_eq!(
        blocks[0].opener_code.as_deref(),
        Some("root = Card(\"中文\")")
    );
    assert!(!blocks[1].closed);
    assert!(blocks[1].opener_code.is_none());
}

#[test]
fn scanner_ignores_other_blocks_inline_fences_and_string_fences() {
    let input = "```rust\n```gen-ui\nroot = Bad()\n```\ninline ```gen-ui```\n```gen-ui\nroot = Text(\"line\n```\nend\")\n# ```\n```\n";
    let blocks = scan_blocks(input);
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].closed);
    assert!(blocks[0].source.contains("line\n```\nend"));
    assert!(scan_blocks("    ```gen-ui\nroot = Text(\"no\")").is_empty());
}

#[test]
fn tilde_fence_opener_allows_backticks_in_same_line_code() {
    let input = "~~~gen-ui root = Text(\"```\")\n~~~\n";
    let blocks = scan_blocks(input);
    assert_eq!(blocks.len(), 1);
    assert!(blocks[0].closed);
    assert!(blocks[0].source.contains("root = Text(\"```\")"));
    assert_eq!(
        blocks[0].opener_code.as_deref(),
        Some("root = Text(\"```\")")
    );
}

#[test]
fn prompt_only_advertises_implemented_capabilities() {
    let prompt = system_prompt(&library(), &PromptOptions::default());
    assert!(prompt.contains("```gen-ui\nroot = Stack("));
    assert!(prompt.contains("第一条语句必须是 root"));
    for name in [
        "@Count",
        "@Each",
        "@Set",
        "@Reset",
        "@OpenUrl",
        "@ToAssistant",
    ] {
        assert!(prompt.contains(name));
    }
    for name in [
        "Query",
        "Mutation",
        "MCP",
        "tools",
        "@Run",
        "@Upper",
        "@CopyToClipboard",
    ] {
        assert!(!prompt.contains(name), "{name}");
    }
}

#[test]
fn state_default_errors_use_the_state_statement_id() {
    let lib = library();
    let result = parse("root = Stack([])\n$bad = Text()", &lib);
    assert!(result.errors.iter().any(|error| {
        error.code == ErrorCode::MissingRequired && error.statement_id.as_deref() == Some("$bad")
    }));
    assert!(!result
        .errors
        .iter()
        .any(|error| error.statement_id.as_deref() == Some("root")));
}

#[test]
fn error_and_expression_serialization_roundtrip() {
    let lib = library();
    let result = parse("root = Text($x + 1)", &lib);
    let expr = result.statements["root"].expr.clone();
    let value = serde_json::to_value(&expr).unwrap();
    assert_eq!(
        serde_json::from_value::<peri_genui::Expr>(value).unwrap(),
        expr
    );
    let result = parse("root = Text()", &lib);
    let value = serde_json::to_value(&result.errors[0]).unwrap();
    assert_eq!(value["code"], "missing-required");
    assert_eq!(value["statementId"], "root");
    assert_eq!(
        serde_json::from_value::<peri_genui::ParseError>(value).unwrap(),
        result.errors[0]
    );
}

#[test]
fn mixed_type_sort_does_not_require_a_total_order() {
    let lib = library();
    let values = [
        json!(2),
        json!(10),
        json!("1x"),
        json!(null),
        json!(true),
        json!({}),
        json!("中文"),
        json!("é"),
        json!("E"),
    ];
    for offset in 0..values.len() {
        let data = (0..128)
            .map(|i| values[(i + offset) % values.len()].clone())
            .collect::<Vec<_>>();
        let source = format!(
            "root = Table([Col(\"mixed\", @Sort({data}, \"\"))])",
            data = serde_json::to_string(&data).unwrap()
        );
        let actual = parse(&source, &lib).to_fixture_json();
        assert_eq!(actual["errors"], json!([]));
        assert_eq!(
            actual["root"]["props"]["columns"][0]["props"]["data"]
                .as_array()
                .unwrap()
                .len(),
            128
        );
    }
}

#[test]
fn adversarial_depth_long_operators_cycles_and_unicode_do_not_panic() {
    let lib = library();
    for source in [
        format!("root = Text({}1{})", "(".repeat(2000), ")".repeat(2000)),
        format!(
            "root = Text({})",
            std::iter::repeat_n("1", 5000).collect::<Vec<_>>().join("+")
        ),
        "root = Stack([a])\na = [b,b]\nb = [a,a]".into(),
        "root = Text(\"中文\u{1f600}\\uD83D\\uDE00\")".into(),
        "root = Text('unclosed\\".into(),
        "root = Stack([Text(\"a\"), @Each([1,2], \"x\", Text(x))])".into(),
    ] {
        let parsed = parse(&source, &lib);
        let _ = parsed.to_fixture_json();
    }
    let nested = format!(
        "root = $show ? {}Text(\"end\"){} : null\n$show = true",
        "Stack([".repeat(20),
        "])".repeat(20)
    );
    assert!(parse(&nested, &lib)
        .errors
        .iter()
        .any(|e| e.code == ErrorCode::ParseFailed));
    let source = format!(
        "root = a0\n{}",
        (0..200)
            .map(|i| format!("a{i} = a{}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(parse(&source, &lib)
        .errors
        .iter()
        .any(|e| e.code == ErrorCode::ParseFailed));
}
