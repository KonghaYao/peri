#[test]
fn test_index_html_contains_terminal_div() {
    // index() 直接返回 include_str!("../index.html")，测试源文件即可覆盖内容
    let html = include_str!("../index.html");
    assert!(
        html.contains("<div id=\"terminals\">"),
        "index.html 应包含 <div id=\"terminals\">"
    );
}
