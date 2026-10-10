//! `resources::frontmatter` 的解析证据：分隔符识别、verbatim JSON 转换与
//! name/description 门闩。

use super::*;
use serde_json::json;

#[test]
fn split_frontmatter_requires_first_line_delimiter() {
    let (fm, body) = split_frontmatter("---\nname: a\n---\nbody\n").expect("合法 frontmatter");
    assert_eq!(fm, "name: a\n");
    assert_eq!(body, "body\n");

    // BOM 前缀被忽略。
    let (fm, _) = split_frontmatter("\u{feff}---\nname: b\n---\n").expect("BOM 前缀");
    assert_eq!(fm, "name: b\n");

    // 首行不是 `---` → 无 frontmatter。
    assert!(split_frontmatter("name: a\n---\n").is_none());
    // 未闭合 → 无 frontmatter。
    assert!(split_frontmatter("---\nname: a\n").is_none());
    // 分隔行允许尾随空白。
    let (fm, _) = split_frontmatter("---   \nname: c\n---   \nx").expect("尾随空白");
    assert_eq!(fm, "name: c\n");
    // 无换行结尾的闭合行。
    let (_, body) = split_frontmatter("---\nname: d\n---").expect("无尾换行");
    assert_eq!(body, "");
}

#[test]
fn frontmatter_json_keeps_unknown_fields_verbatim() {
    let map = frontmatter_json(
        "---\n\
         name: demo\n\
         description: A demo skill\n\
         custom-field:\n  - kept\n  - verbatim\n\
         count: 3\n\
         ratio: 1.5\n\
         enabled: true\n\
         nothing: null\n\
         nested:\n  key: value\n\
         ---\nbody\n",
    )
    .expect("解析成功");
    assert_eq!(map.get("name"), Some(&json!("demo")));
    assert_eq!(map.get("custom-field"), Some(&json!(["kept", "verbatim"])));
    assert_eq!(map.get("count"), Some(&json!(3)));
    assert_eq!(map.get("ratio"), Some(&json!(1.5)));
    assert_eq!(map.get("enabled"), Some(&json!(true)));
    assert_eq!(map.get("nothing"), Some(&json!(null)));
    assert_eq!(map.get("nested"), Some(&json!({"key": "value"})));
}

#[test]
fn frontmatter_json_handles_tagged_scalars() {
    let map = frontmatter_json("---\nname: !custom tagged\n---\n").expect("tagged 值可解析");
    assert_eq!(map.get("name"), Some(&json!("tagged")));
}

#[test]
fn frontmatter_json_rejects_non_mapping_and_invalid_yaml() {
    assert!(
        frontmatter_json("---\n- a\n- b\n---\n").is_none(),
        "顶层序列不是 mapping"
    );
    assert!(
        frontmatter_json("---\nfoo: [unclosed\n---\n").is_none(),
        "非法 YAML"
    );
    assert!(frontmatter_json("no frontmatter").is_none());
}

#[test]
fn frontmatter_summary_requires_name_and_description_and_trims() {
    let map =
        frontmatter_json("---\nname: demo\ndescription: |\n  first line\n  second line\n---\n")
            .expect("解析成功");
    let (name, description) = frontmatter_summary(&map).expect("摘要");
    assert_eq!(name, "demo");
    assert_eq!(
        description, "first line\nsecond line",
        "description 仅 trim 尾随空白（块标量的尾随换行被去掉）"
    );

    let missing_description = frontmatter_json("---\nname: demo\n---\n").expect("解析成功");
    assert!(frontmatter_summary(&missing_description).is_none());

    let empty_name = frontmatter_json("---\nname: ''\ndescription: d\n---\n").expect("解析成功");
    assert!(frontmatter_summary(&empty_name).is_none());

    let non_string = frontmatter_json("---\nname: [a]\ndescription: d\n---\n").expect("解析成功");
    assert!(frontmatter_summary(&non_string).is_none());
}
