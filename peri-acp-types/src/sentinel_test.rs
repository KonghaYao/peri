use super::*;

#[test]
fn formatter_emits_canonical_v1_body() {
    assert_eq!(
        format_projection_sentinel_v1(996),
        "... [996 字符已省略] ..."
    );
}

#[test]
fn scanner_accepts_lf_crlf_and_eof_lines() {
    let text = "... [1 字符已省略] ...\n... [2 字符已省略] ...\r\n... [3 字符已省略] ...";
    let counts = projection_sentinel_counts_v1(text);

    assert_eq!(counts.get("... [1 字符已省略] ..."), Some(&1));
    assert_eq!(counts.get("... [2 字符已省略] ..."), Some(&1));
    assert_eq!(counts.get("... [3 字符已省略] ..."), Some(&1));
}

#[test]
fn scanner_requires_complete_logical_lines() {
    let text = concat!(
        "prefix ... [1 字符已省略] ...\n",
        "... [2 字符已省略] ... suffix\n",
        " ... [3 字符已省略] ...\n",
        "... [4 字符已省略] ... \n",
        "... [5 字符已省略] ...\rnext",
    );

    assert!(projection_sentinel_counts_v1(text).is_empty());
}

#[test]
fn scanner_requires_ascii_digits_and_fixed_punctuation() {
    let text = concat!(
        "... [ 字符已省略] ...\n",
        "... [１２ 字符已省略] ...\n",
        "... [+12 字符已省略] ...\n",
        "... [12  字符已省略] ...\n",
        "… [12 字符已省略] ...\n",
    );

    assert!(projection_sentinel_counts_v1(text).is_empty());
}

#[test]
fn scanner_does_not_parse_arbitrarily_long_digits() {
    let digits = "9".repeat(100_000);
    let sentinel = format!("... [{digits} 字符已省略] ...");
    let counts = projection_sentinel_counts_v1(&sentinel);

    assert_eq!(counts.get(&sentinel), Some(&1));
}

#[test]
fn scanner_handles_arbitrary_utf8_without_panicking() {
    let text = "前缀🦀\n... [7 字符已省略] ...\r\n尾部é\r内容";
    let counts = projection_sentinel_counts_v1(text);

    assert_eq!(counts.get("... [7 字符已省略] ..."), Some(&1));
}

#[test]
fn delta_normalizes_line_endings_but_distinguishes_digits() {
    let pre = "... [12 字符已省略] ...\r\n";

    assert!(!introduces_projection_sentinel_v1(
        pre,
        "... [12 字符已省略] ...\n"
    ));
    assert!(introduces_projection_sentinel_v1(
        pre,
        "... [13 字符已省略] ...\n"
    ));
}

#[test]
fn delta_compares_occurrence_multiplicity() {
    let sentinel = format_projection_sentinel_v1(12);
    let pre = format!("{sentinel}\n");
    let post = format!("{sentinel}\n{sentinel}\n");

    assert!(introduces_projection_sentinel_v1(&pre, &post));
    assert!(!introduces_projection_sentinel_v1(&post, &pre));
}
