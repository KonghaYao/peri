use std::sync::Arc;

use peri_genui::{
    evaluate_root, parse, scan_blocks, system_prompt, ErrorCode, EvalContext, EvalError, Library,
    PromptOptions, StatementKind, StreamParser,
};
use serde_json::{json, Value};

fn library() -> Library {
    Library::from_json_str(include_str!("fixtures/1.0/library.json")).unwrap()
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
