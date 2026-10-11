use super::*;

#[test]
fn powershell_missing_command_requires_its_specific_error_id_and_nonzero_exit() {
    let output = "gti : The term 'gti' is not recognized as the name of a cmdlet\n    + FullyQualifiedErrorId : CommandNotFoundException\n";
    let hint = command_not_found_hint("& gti status", output, Some(1)).unwrap();
    assert!(hint.contains("Command `gti` not found in PATH"), "{hint}");
    assert!(command_not_found_hint("gti", output, Some(0)).is_none());
    assert!(command_not_found_hint("gti", output, None).is_none());
    let ordinary_error = output.replace("CommandNotFoundException", "ItemNotFoundException");
    assert!(command_not_found_hint("gti", &ordinary_error, Some(1)).is_none());
}

/// [触发] exit 127 + `command not found`：有相似候选时给出 Did you mean。
#[test]
fn test_hint_suggests_similar_candidate() {
    // dockr 是 docker 的丢字符拼错（Skim 子序列匹配），dotnet 不含 c 不命中
    let hint = build_hint("dockr", &["docker".to_string(), "dotnet".to_string()]);
    assert!(hint.contains("not found in PATH"), "{hint}");
    assert!(hint.contains("Did you mean: docker"), "{hint}");
}

/// [门槛] 退出码不是 127（命令找不到以外的一般失败）不产生建议。
#[test]
fn test_hint_requires_exit_code_127() {
    let output = "ls: /nonexistent: No such file or directory\n[Exit code: 1]";
    assert!(command_not_found_hint("ls /nonexistent", output, Some(1)).is_none());
    assert!(command_not_found_hint("ls /nonexistent", output, None).is_none());
}

/// [门槛] 127 但没有 shell 的缺失命令消息（脚本自定义退出码）不产生建议。
#[test]
fn test_hint_requires_command_not_found_message() {
    assert!(
        command_not_found_hint("exit 127", "custom failure\n[Exit code: 127]", Some(127)).is_none()
    );
}

/// [兜底] 无相似候选时不硬凑 Did you mean，回退环境类诊断。
#[test]
fn test_hint_falls_back_to_env_diagnosis() {
    let hint = build_hint("xx_q1w2e3_not_a_real_cmd_xx", &["git".to_string()]);
    assert!(!hint.contains("Did you mean"), "{hint}");
    assert!(hint.contains("not found in PATH"), "{hint}");
    assert!(hint.contains("PATH / conda / venv"), "{hint}");
}

/// [噪声] 短查询对泛化子序列候选（xy→xylophone）不得硬凑。
#[test]
fn test_hint_rejects_noise_candidates() {
    let hint = build_hint("xy", &["xylophone".to_string()]);
    assert!(!hint.contains("Did you mean"), "{hint}");
}

/// [解析] zsh 形态：`command not found: NAME`。
#[test]
fn test_extract_missing_command_zsh() {
    let output = "zsh:1: command not found: gti";
    assert_eq!(extract_missing_command(output, "gti status"), "gti");
}

/// [解析] bash 形态：`NAME: command not found`（含 `line 1` 前缀）。
#[test]
fn test_extract_missing_command_bash() {
    let output = "bash: line 1: gti: command not found";
    assert_eq!(extract_missing_command(output, "gti status"), "gti");
    let output = "bash: gti: command not found";
    assert_eq!(extract_missing_command(output, "gti"), "gti");
}

/// [解析] sudo 形态：提取被代理命令名而非 `sudo` 本身。
#[test]
fn test_extract_missing_command_sudo() {
    let output = "sudo: gti: command not found";
    assert_eq!(extract_missing_command(output, "sudo gti status"), "gti");
}

/// [解析] 未识别格式回退命令首词。
#[test]
fn test_extract_missing_command_fallback() {
    let output = "something went wrong";
    assert_eq!(extract_missing_command(output, "gti status"), "gti");
}

/// [扫描] 目录缺失跳过、条目去重、跨目录收集。
#[test]
fn test_scan_path_executables_in_dedup_and_missing_dir() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    std::fs::write(first.path().join("alpha"), "").unwrap();
    std::fs::write(first.path().join("beta"), "").unwrap();
    std::fs::write(second.path().join("beta"), "").unwrap();
    std::fs::write(second.path().join("gamma"), "").unwrap();
    let path_env = std::env::join_paths([
        first.path(),
        std::path::Path::new("/definitely/not/a/real/dir"),
        second.path(),
    ])
    .unwrap();
    let names = scan_path_executables_in(Some(path_env));
    let mut names = names;
    names.sort();
    assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    assert!(scan_path_executables_in(None).is_empty());
}

/// [端到端] 真实 PATH：`carg`（`cargo` 的丢字符拼错）触发建议并给出
/// `not found in PATH` 说明。候选是否包含 `cargo` 取决于真实 PATH 的长度
/// 与扫描预算，不做强断言（候选质量由 `build_hint` 的单元测试锁定）。
#[test]
fn test_hint_end_to_end_with_real_path() {
    let output = "zsh:1: command not found: carg\n[Exit code: 127]";
    let hint = command_not_found_hint("carg build", output, Some(127))
        .expect("command not found + exit 127 应产生建议");
    assert!(hint.contains("not found in PATH"), "{hint}");
}
