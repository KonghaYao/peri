use std::{fs, path::PathBuf, sync::Arc};

use indexmap::IndexMap;
use peri_genui::{parse, scan_blocks, EvalContext, Library, ParseResult, StreamParser};
use serde_json::{json, Value};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/1.0")
}
fn read(path: &str) -> String {
    fs::read_to_string(fixtures().join(path)).unwrap()
}
fn library() -> Arc<Library> {
    Arc::new(Library::from_json_str(&read("library.json")).unwrap())
}
fn context() -> EvalContext {
    let mut context = EvalContext::default();
    context.functions.insert(
        "Upper".into(),
        Arc::new(|args: &IndexMap<String, Value>| {
            Ok(json!(args
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_uppercase()))
        }),
    );
    context
}

fn normalize(input: &str) -> String {
    let adapted = input
        .replace("```openui-lang", "```gen-ui")
        .replace("```openui", "```gen-ui");
    let blocks = scan_blocks(&adapted);
    if blocks.is_empty() {
        input.to_owned()
    } else {
        blocks
            .into_iter()
            .map(|b| b.source)
            .collect::<Vec<_>>()
            .join("\n")
    }
}

fn claims(expected: &[Value], actual: &[Value], exact: bool) -> bool {
    if exact && expected.len() != actual.len() {
        return false;
    }
    let mut pool = actual.to_vec();
    let mut expected = expected.to_vec();
    expected.sort_by_key(|e| std::cmp::Reverse(e.as_object().unwrap().len()));
    for entry in expected {
        let Some(at) = pool.iter().position(|got| {
            entry
                .as_object()
                .unwrap()
                .iter()
                .all(|(k, v)| got.get(k).unwrap_or(&Value::Null) == v)
        }) else {
            return false;
        };
        pool.remove(at);
    }
    true
}

fn compare(id: &str, expected: &Value, result: &ParseResult) {
    let actual = result.to_fixture_json_with_context(&context());
    for key in [
        "root",
        "errors",
        "unresolved",
        "orphans",
        "incomplete",
        "state",
        "errorObjects",
    ] {
        let Some(want) = expected.get(key) else {
            continue;
        };
        let got = &actual[key];
        let matches = match key {
            "errors" | "errorObjects" => claims(
                want.as_array().unwrap(),
                got.as_array().unwrap(),
                key == "errors",
            ),
            "unresolved" | "orphans" => {
                let as_set = |v: &Value| {
                    v.as_array()
                        .unwrap()
                        .iter()
                        .map(Value::to_string)
                        .collect::<std::collections::BTreeSet<_>>()
                };
                as_set(want) == as_set(got)
            }
            _ => want == got,
        };
        assert!(
            matches,
            "{id}: {key}\nexpected: {want}\nactual: {got}\nfull: {actual}"
        );
    }
}

fn run(id: &str) {
    let library = library();
    let expected: Value = serde_json::from_str(&read(&format!("{id}/expected.json"))).unwrap();
    let chunks_path = format!("{id}/chunks.json");
    if fixtures().join(&chunks_path).exists() {
        let chunks: Vec<String> = serde_json::from_str(&read(&chunks_path)).unwrap();
        let mut raw = String::new();
        let mut stream = StreamParser::new(library.clone());
        for chunk in &chunks {
            raw.push_str(chunk);
            stream.set(&normalize(&raw), false);
        }
        compare(id, &expected, &stream.finish());
        compare(
            id,
            &expected,
            &parse(&normalize(&chunks.concat()), &library),
        );
    } else {
        compare(
            id,
            &expected,
            &parse(&normalize(&read(&format!("{id}/input.oui"))), &library),
        );
    }
}

macro_rules! fixture {
    ($name:ident, $id:literal) => {
        #[test]
        fn $name() {
            run($id);
        }
    };
    ($name:ident, $id:literal, $reason:literal) => {
        #[test]
        #[ignore = $reason]
        fn $name() {
            run($id);
        }
    };
}

fixture!(grammar_fences, "grammar/003-fence-multiple-joined");
fixture!(grammar_comments, "grammar/004-comment-strip");
fixture!(grammar_escapes, "grammar/005-lex-escapes");
fixture!(grammar_minus, "grammar/006-lex-minus-and-skip");
fixture!(grammar_precedence, "grammar/008-expr-precedence-and-keys");
fixture!(grammar_member_digits, "grammar/009-member-digits-field");
fixture!(grammar_tree, "grammar/010-roundtrip-tree");
fixture!(entry_root, "entry/001-root-statement");
fixture!(entry_first, "entry/002-first-calls-root-component");
fixture!(entry_later, "entry/003-root-component-later");
fixture!(entry_not_component, "entry/004-root-not-a-component");
fixture!(entry_state_first, "entry/005-first-is-state-declaration");
fixture!(entry_ternary, "entry/006-ternary-root-multiline");
fixture!(entry_no_component, "entry/007-no-root-component-call");
fixture!(
    errors_unknown_component,
    "errors/001-unknown-component-dropped"
);
fixture!(errors_unknown_function, "errors/002-unknown-function-null");
fixture!(errors_unresolved_root, "errors/003-no-root-unresolved-root");
fixture!(
    errors_missing_required,
    "errors/005-shape-missing-required-metric"
);
fixture!(errors_null_required, "errors/006-null-required-dropped");
fixture!(errors_enum, "errors/007-type-mismatch-enum");
fixture!(errors_list, "errors/008-type-mismatch-bare-list");
fixture!(errors_excess, "errors/009-excess-args-dropped");
fixture!(errors_inline_reserved, "errors/010-inline-reserved-array");
fixture!(errors_array, "errors/012-validate-array-elements");
fixture!(
    errors_component_slot,
    "errors/013-validate-component-slot-data"
);
fixture!(errors_function_args, "errors/014-function-literal-args");
fixture!(
    evaluation_kind,
    "evaluation/001-kind-classify",
    "包含 Query 声明；本阶段求值必须显式 unsupported"
);
fixture!(evaluation_cycle, "evaluation/003-ref-cycle");
fixture!(evaluation_duplicate, "evaluation/004-duplicate-later-wins");
fixture!(evaluation_coerce, "evaluation/005-op-coerce-values");
fixture!(evaluation_member, "evaluation/006-member-pluck-and-null");
fixture!(evaluation_sort, "evaluation/008-builtin-sort-locale");
fixture!(evaluation_function, "evaluation/010-function-registered");
fixture!(
    evaluation_function_null,
    "evaluation/011-function-null-required-arg"
);
fixture!(
    evaluation_query_dollar,
    "evaluation/012-kind-query-dollar-name",
    "Query 求值不以默认数据掩盖 unsupported"
);
fixture!(actions_several, "actions/001-several-actions");
fixture!(actions_one, "actions/002-one-action");
fixture!(actions_context, "actions/003-event-context-any-value");
fixture!(
    actions_form_state,
    "actions/004-event-form-state-per-action"
);
fixture!(
    actions_query,
    "actions/006-query-rerun-on-state",
    "Query 调度不在本阶段"
);
fixture!(
    actions_mutation,
    "actions/007-mutation-run",
    "Mutation 执行不在本阶段；Run 描述另有测试"
);
fixture!(
    actions_missing_tool,
    "actions/008-tool-not-found-stops-plan",
    "宿主动作执行与失败中断不在本阶段"
);
fixture!(actions_custom, "actions/009-custom-action");
fixture!(actions_reference, "actions/010-one-action-by-reference");
fixture!(actions_list, "actions/011-action-list-is-data");
fixture!(state_auto, "state/001-store-auto-declare");
fixture!(state_binding, "state/002-binding-two-way");
fixture!(state_form, "state/003-form-scope");
fixture!(state_page, "state/004-form-page-level");
fixture!(
    state_rules,
    "state/005-validation-rules",
    "结构化表单规则校验属于宿主交互；这里仅校验组件 JSON Schema"
);
fixture!(
    streaming_worked,
    "streaming/001-worked-example",
    "示例依赖 Query 默认结果，改测显式 unsupported"
);
fixture!(
    streaming_ternary,
    "streaming/003-multiline-ternary-across-chunks"
);
fixture!(streaming_fence, "streaming/005-fence-arrives-late");
fixture!(streaming_escape, "streaming/006-truncated-string-escape");

#[test]
fn every_fixture_at_every_character_boundary() {
    let library = library();
    let context = context();
    let mut cases = 0;
    let mut cuts = 0;
    for area in [
        "grammar",
        "entry",
        "errors",
        "evaluation",
        "actions",
        "state",
        "streaming",
    ] {
        for entry in fs::read_dir(fixtures().join(area)).unwrap() {
            let path = entry.unwrap().path();
            if !path.is_dir() || path.join("validation.json").exists() {
                continue;
            }
            let raw = if path.join("chunks.json").exists() {
                serde_json::from_str::<Vec<String>>(
                    &fs::read_to_string(path.join("chunks.json")).unwrap(),
                )
                .unwrap()
                .concat()
            } else {
                fs::read_to_string(path.join("input.oui")).unwrap()
            };
            let source = normalize(&raw);
            let expected = parse(&source, &library).to_fixture_json_with_context(&context);
            for cut in source
                .char_indices()
                .map(|(i, _)| i)
                .chain(std::iter::once(source.len()))
            {
                let mut stream = StreamParser::new(library.clone());
                stream.push(&source[..cut]);
                stream.push(&source[cut..]);
                assert_eq!(
                    stream.finish().to_fixture_json_with_context(&context),
                    expected,
                    "{} at {cut}",
                    path.display()
                );
                cuts += 1;
            }
            let mut stream = StreamParser::new(library.clone());
            for cut in raw.char_indices().map(|(i, c)| i + c.len_utf8()) {
                stream.set(&normalize(&raw[..cut]), false);
            }
            assert_eq!(
                stream.finish().to_fixture_json_with_context(&context),
                expected,
                "{} raw stream",
                path.display()
            );
            cases += 1;
        }
    }
    assert_eq!(cases, 53);
    println!("切分验证：{cases} 个程序，{cuts} 个字符边界，包含逐字符 fence 归一化");
}
