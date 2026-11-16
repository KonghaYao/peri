#[test]
fn account_reference_accepts_only_bounded_ascii_names() {
    for value in ["a", "SYNTHETIC_account-42", &"x".repeat(128)] {
        assert!(super::valid_account_reference(value));
    }
    for value in [
        "",
        "work/team",
        " synthetic",
        "synthetic ",
        "syn thetic",
        "synthetic\n",
        "合成",
        "synthetic.42",
        &"x".repeat(129),
    ] {
        assert!(!super::valid_account_reference(value));
    }
}
