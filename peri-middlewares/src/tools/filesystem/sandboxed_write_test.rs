use super::SandboxedWriteTool;
use peri_agent::tools::BaseTool;

fn make_tool(dir: &tempfile::TempDir, allowed: Vec<&str>) -> SandboxedWriteTool {
    let cwd = dir.path().to_str().unwrap().to_string();
    for d in &allowed {
        std::fs::create_dir_all(dir.path().join(d)).unwrap();
    }
    SandboxedWriteTool::new(cwd, allowed.iter().map(|s| s.to_string()).collect()).unwrap()
}

// ─── P0: name / description / parameters ────────────────────────────

#[test]
fn test_sandboxed_write_tool_name() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    assert_eq!(tool.name(), "Write");
}

#[test]
fn test_sandboxed_write_tool_description_contains_restriction() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox", "output"]);
    let desc = tool.description();
    assert!(
        desc.contains("Restriction"),
        "desc should mention restriction: {}",
        desc
    );
    assert!(
        desc.contains("sandbox"),
        "desc should list sandbox dirs: {}",
        desc
    );
    assert!(
        desc.contains("output"),
        "desc should list output dir: {}",
        desc
    );
    assert!(
        desc.contains("atomic write"),
        "desc should include original Write description"
    );
}

#[test]
fn test_sandboxed_write_parameters_delegates_to_inner() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let params = tool.parameters();
    assert!(
        params["properties"]["append"].is_object(),
        "SandboxedWrite should include append param"
    );
    assert!(params["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v.as_str() == Some("file_path")));
}

// ─── P0: path validation ────────────────────────────────────────────

#[tokio::test]
async fn test_sandboxed_write_invoke_normal() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["plans"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "plans/report.md", "content": "# Report"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(result.contains("Wrote 1 line"), "unexpected: {}", result);
    let content = std::fs::read_to_string(dir.path().join("plans/report.md")).unwrap();
    assert_eq!(content, "# Report");
}

#[tokio::test]
async fn test_sandboxed_write_invoke_absolute_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let abs = dir.path().join("outside.txt");
    let result = tool
        .invoke(
            serde_json::json!({"file_path": abs.to_str().unwrap(), "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "absolute path should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Absolute"),
        "error should mention absolute: {}",
        err
    );
}

#[tokio::test]
async fn test_sandboxed_write_invoke_traversal_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/../outside.txt", "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), ".. traversal should be rejected");
}

#[tokio::test]
async fn test_sandboxed_write_invoke_outside_dir_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "other/outside.txt", "content": "nope"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_err(), "path outside sandbox should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("outside") || err.contains("allowed"),
        "error should indicate restriction: {}",
        err
    );
}

// ─── P1: append support ─────────────────────────────────────────────

#[tokio::test]
async fn test_sandboxed_write_append_mode() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["plans"]);
    // First write
    tool.invoke(
        serde_json::json!({"file_path": "plans/chunked.md", "content": "line1\n"}),
        peri_agent::tools::ToolContext::new(&[], "."),
    )
    .await
    .unwrap();
    // Append
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "plans/chunked.md", "content": "line2\n", "append": true}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await
        .unwrap();
    assert!(
        result.contains("Appended"),
        "should use append mode: {}",
        result
    );
    let content = std::fs::read_to_string(dir.path().join("plans/chunked.md")).unwrap();
    assert_eq!(content, "line1\nline2\n");
}

// ─── P0: guard unit tests ───────────────────────────────────────────

#[test]
fn test_validate_sandbox_path_relative_ok() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let sandbox_root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/hello.md",
        &[sandbox_root],
    );
    assert!(result.is_ok(), "relative path should pass: {:?}", result.err());
}

#[test]
fn test_validate_sandbox_path_absolute_rejected() {
    use super::super::sandbox_guard::{validate_sandbox_path, SandboxPathError};
    let result = validate_sandbox_path("/tmp", "/etc/passwd", &[]);
    assert!(result.is_err());
    match result.unwrap_err() {
        SandboxPathError::AbsolutePath { .. } => {}
        other => panic!("expected AbsolutePath, got: {:?}", other),
    }
}

// ═══════════════════════════════════════════════════════════════════════
// ADVERSARIAL TESTS — Red-team verification of sandbox path validation
// ═══════════════════════════════════════════════════════════════════════

use std::os::unix::fs as unix_fs;

// ─── Vector 1: Path Traversal Variants ───────────────────────────────

// The segment check on line 52 compares each segment == ".." exactly.
// Segments like "....", ".. ", ". .", "..\r" all bypass this check.
// On Unix, none of these are actually interpreted as parent directory,
// so they're harmless — but worth verifying the defense.

#[test]
fn test_adv_quad_dot_bypass() {
    use super::super::sandbox_guard::{validate_sandbox_path, SandboxPathError};
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "....//secret.txt",
        &[root],
    );
    assert!(result.is_err(), "....// should be blocked by sandbox boundary");
    match result.unwrap_err() {
        SandboxPathError::OutsideSandbox { .. } => {}
        SandboxPathError::PathTraversal { .. } => {
            panic!("....// is not a real traversal, should not be caught as PathTraversal")
        }
        other => panic!("unexpected error variant: {:?}", other),
    }
}

#[test]
fn test_adv_backslash_normalization_blocks_traversal() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/..\\outside.txt",
        &[root],
    );
    assert!(
        result.is_err(),
        "..\\\\ should be normalized and caught: {:?}",
        result.err()
    );
    assert!(
        result.unwrap_err().to_string().contains("traversal"),
        "should report traversal"
    );
}

#[test]
fn test_adv_dot_slash_dot_not_traversal() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        ".\\/.",
        &[root],
    );
    assert!(result.is_err(), ".\\\\. should be outside sandbox: {:?}", result.err());
}

#[test]
fn test_adv_double_dot_substring_not_traversal() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/foo..bar.txt",
        &[root],
    );
    assert!(
        result.is_ok(),
        "\"..\" as substring should NOT be caught: {:?}",
        result.err()
    );
}

#[test]
fn test_adv_dot_dot_space_not_caught() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/.. /secret.txt",
        &[root],
    );
    assert!(
        result.is_ok(),
        "\".. \" (with space) bypasses segment check but is harmless on Unix: {:?}",
        result.err()
    );
}

#[test]
fn test_adv_leading_dots_file() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let r1 = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/..config",
        &[root.clone()],
    );
    assert!(
        r1.is_ok(),
        "hidden file starting with dots should be allowed: {:?}",
        r1.err()
    );
    let r2 = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/.env",
        &[root],
    );
    assert!(
        r2.is_ok(),
        "hidden file should be allowed: {:?}",
        r2.err()
    );
}

// ─── Vector 2: Absolute Path Bypass Attempts ──────────────────────────

#[test]
fn test_adv_space_prefix_not_absolute() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/ /etc/passwd",
        &[root],
    );
    assert!(
        result.is_ok(),
        "space-prefixed path should be safe: {:?}",
        result.err()
    );
}

#[test]
fn test_adv_tilde_with_dotdot_caught() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "~/../../etc/passwd",
        &[root],
    );
    assert!(result.is_err(), "~ path with .. should be caught");
}

// ─── Vector 3: Symlink Attacks ────────────────────────────────────────

#[test]
fn test_adv_symlink_inside_sandbox_to_external_blocked() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    let sandbox = dir.path().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let symlink = sandbox.join("escape_hatch");
    let target = dir.path().join("outside_secrets");
    std::fs::create_dir_all(&target).unwrap();
    unix_fs::symlink(&target, &symlink).unwrap();
    let root = sandbox.canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/escape_hatch/secret.txt",
        &[root],
    );
    assert!(
        result.is_err(),
        "symlink to external dir should be BLOCKED. Got: {:?}",
        result.ok()
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("outside") || err.contains("allowed"),
        "should report outside sandbox: {}",
        err
    );
}

#[test]
fn test_adv_symlink_inside_sandbox_to_sibling_allowed() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    let sandbox = dir.path().join("sandbox");
    std::fs::create_dir_all(sandbox.join("sub_a")).unwrap();
    std::fs::create_dir_all(sandbox.join("sub_b")).unwrap();
    unix_fs::symlink("../sub_b", sandbox.join("sub_a").join("link")).unwrap();
    let root = sandbox.canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/sub_a/link/file.txt",
        &[root],
    );
    assert!(
        result.is_ok(),
        "symlink to sibling inside sandbox should be ALLOWED: {:?}",
        result.err()
    );
}

#[test]
fn test_adv_symlink_chain_outside_blocked() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    let sandbox = dir.path().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(sandbox.join("b")).unwrap();
    unix_fs::symlink(&outside, sandbox.join("b").join("link_to_outside")).unwrap();
    unix_fs::symlink("b/link_to_outside", sandbox.join("a")).unwrap();
    let root = sandbox.canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/a/secret.txt",
        &[root],
    );
    assert!(
        result.is_err(),
        "symlink chain to external dir should be BLOCKED. Got: {:?}",
        result.ok()
    );
}

// ─── Vector 4: TOCTOU Race (Ancestor Check vs Inner Write) ────────────

/// This test documents a TOCTOU window between validate_sandbox_path (which
/// canonicalizes ancestors) and WriteFileTool::resolve_path (which does its
/// own canonicalization). If a directory is replaced with a symlink between
/// these two calls, the write can escape the sandbox.
///
/// This is difficult to exploit reliably, but the window exists.
#[test]
fn test_adv_toctou_window_documented() {
    use super::super::sandbox_guard::validate_sandbox_path;
    use super::super::resolve_path;

    let dir = tempfile::tempdir().unwrap();
    let sandbox = dir.path().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let root = sandbox.canonicalize().unwrap();

    let validated =
        validate_sandbox_path(&dir.path().to_string_lossy(), "sandbox/normal.txt", &[root])
            .unwrap();
    assert!(validated.to_string_lossy().contains("sandbox"));

    let outside_raw = dir.path().join("outside");
    std::fs::create_dir_all(&outside_raw).unwrap();
    let outside = outside_raw.canonicalize().unwrap();

    let sandbox_sub = sandbox.join("sub");
    std::fs::create_dir_all(&sandbox_sub).unwrap();

    // After validation passes, replace sub/ with symlink to outside
    std::fs::remove_dir_all(&sandbox_sub).unwrap();
    unix_fs::symlink(&outside, &sandbox_sub).unwrap();

    // Now resolve_path would canonicalize through the symlink
    let cwd_str = dir.path().to_string_lossy().to_string();
    let full_path = format!("{}/sandbox/sub/secret.txt", cwd_str);
    let resolved = resolve_path(&cwd_str, &full_path);
    assert!(
        resolved.starts_with(&outside),
        "resolve_path follows symlinks: resolved to {:?} (expected under {:?})",
        resolved, outside
    );

    // Cleanup for other tests
    drop(dir);
}

// ─── Vector 5: Unicode Homoglyphs & Control Chars ─────────────────────

#[test]
fn test_adv_rtl_override_bypasses_segment_check() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let rtl_path = format!("sandbox/\u{202E}../outside");
    let result = validate_sandbox_path(&dir.path().to_string_lossy(), &rtl_path, &[root]);
    assert!(
        result.is_ok(),
        "RTL override before .. bypasses segment check: {:?}",
        result.err()
    );
}

#[test]
fn test_adv_zero_width_bypasses_segment_check() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let zw_path = format!("sandbox/\u{200B}..\u{200D}/file.txt");
    let result = validate_sandbox_path(&dir.path().to_string_lossy(), &zw_path, &[root]);
    assert!(
        result.is_ok(),
        "zero-width chars around .. bypass segment check: {:?}",
        result.err()
    );
}

#[test]
fn test_adv_fullwidth_dots_bypass() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let fw_path = "sandbox/\u{FF0E}\u{FF0E}/file.txt";
    let result = validate_sandbox_path(&dir.path().to_string_lossy(), fw_path, &[root]);
    assert!(
        result.is_ok(),
        "fullwidth dots bypass segment check: {:?}",
        result.err()
    );
}

// ─── Vector 6: Case Sensitivity ───────────────────────────────────────

#[test]
fn test_adv_case_variant_canonicalizes() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    let sandbox = dir.path().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let root = sandbox.canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "SANDBOX/file.txt",
        &[root],
    );
    assert!(
        result.is_ok(),
        "case variant should canonicalize to correct case on macOS: {:?}",
        result.err()
    );
}

// ─── Vector 7: Null Byte Injection ────────────────────────────────────

#[test]
fn test_adv_null_byte_in_path() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let null_path = "sandbox/file\0../outside";
    let result = validate_sandbox_path(&dir.path().to_string_lossy(), null_path, &[root.clone()]);
    match result {
        Ok(path) => {
            assert!(
                path.starts_with(&root),
                "null byte path should stay in sandbox: {:?}",
                path
            );
        }
        Err(_) => {}
    }
}

// ─── Vector 8: Empty / Special Paths ──────────────────────────────────

#[test]
fn test_adv_empty_path_rejected() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(&dir.path().to_string_lossy(), "", &[root]);
    assert!(result.is_err(), "empty path should be rejected (not inside sandbox)");
}

#[test]
fn test_adv_dot_path_rejected() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(&dir.path().to_string_lossy(), ".", &[root]);
    assert!(result.is_err(), "\".\" should be rejected: outside sandbox");
}

#[test]
fn test_adv_double_slash_allowed() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox//file.txt",
        &[root],
    );
    assert!(result.is_ok(), "double slash should be allowed: {:?}", result.err());
}

#[test]
fn test_adv_trailing_slash_allowed() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let result = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox/file.txt/",
        &[root],
    );
    assert!(result.is_ok(), "trailing slash should be allowed: {:?}", result.err());
}

// ─── Vector 9: Deep Path ──────────────────────────────────────────────

#[test]
fn test_adv_deeply_nested_allowed() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox")).unwrap();
    let root = dir.path().join("sandbox").canonicalize().unwrap();
    let deep = "sandbox/".to_string()
        + &(0..20).map(|i| format!("level_{}/", i)).collect::<String>()
        + "file.txt";
    let result = validate_sandbox_path(&dir.path().to_string_lossy(), &deep, &[root]);
    assert!(result.is_ok(), "deeply nested path should be allowed: {:?}", result.err());
}

// ─── Vector 10: Multiple Sandbox Roots ────────────────────────────────

#[test]
fn test_adv_multiple_sandbox_roots() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox_a")).unwrap();
    std::fs::create_dir_all(dir.path().join("sandbox_b")).unwrap();
    let root_a = dir.path().join("sandbox_a").canonicalize().unwrap();
    let root_b = dir.path().join("sandbox_b").canonicalize().unwrap();

    let r1 = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox_a/file.txt",
        &[root_a.clone(), root_b.clone()],
    );
    assert!(r1.is_ok(), "path in first root: {:?}", r1.err());

    let r2 = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox_b/file.txt",
        &[root_a.clone(), root_b.clone()],
    );
    assert!(r2.is_ok(), "path in second root: {:?}", r2.err());

    let r3 = validate_sandbox_path(
        &dir.path().to_string_lossy(),
        "sandbox_c/file.txt",
        &[root_a, root_b],
    );
    assert!(r3.is_err(), "path outside all roots should be rejected");
}

// ─── Vector 11: macOS /tmp symlink normalization ──────────────────────

#[test]
fn test_adv_macos_tmp_symlink_behavior() {
    use super::super::sandbox_guard::validate_sandbox_path;
    let dir = tempfile::tempdir().unwrap();
    let sandbox = dir.path().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let cwd_canon = dir.path().canonicalize().unwrap();
    let root_canon = sandbox.canonicalize().unwrap();
    assert!(
        root_canon.starts_with(&cwd_canon),
        "canonicalized sandbox should be under canonicalized cwd"
    );
    let result = validate_sandbox_path(
        &cwd_canon.to_string_lossy(),
        "sandbox/file.txt",
        &[root_canon],
    );
    assert!(result.is_ok(), "canonicalized paths should work: {:?}", result.err());
}

// ─── Vector 12: Full Integration (SandboxedWriteTool invoke) ──────────

#[tokio::test]
async fn test_adv_invoke_dotdot_space_bypass_but_harmless() {
    // ".. " (trailing space) bypasses segment check.
    // But on Unix, ".. " is NOT a parent dir → write stays in sandbox.
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/.. /file.txt", "content": "test"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "\".. \" should be allowed (harmless on Unix): {:?}", result.err());
}

#[tokio::test]
async fn test_adv_invoke_double_dot_substring_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let tool = make_tool(&dir, vec!["sandbox"]);
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/foo..bar.txt", "content": "test"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(result.is_ok(), "\"..\" as substring should be allowed: {:?}", result.err());
}

#[tokio::test]
async fn test_adv_invoke_symlink_inside_sandbox_to_external_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let sandbox = dir.path().join("sandbox");
    std::fs::create_dir_all(&sandbox).unwrap();
    let target = dir.path().join("outside");
    std::fs::create_dir_all(&target).unwrap();
    unix_fs::symlink(&target, sandbox.join("escape")).unwrap();
    let tool = SandboxedWriteTool::new(
        dir.path().to_str().unwrap().to_string(),
        vec!["sandbox".to_string()],
    )
    .unwrap();
    let result = tool
        .invoke(
            serde_json::json!({"file_path": "sandbox/escape/secret.txt", "content": "evil"}),
            peri_agent::tools::ToolContext::new(&[], "."),
        )
        .await;
    assert!(
        result.is_err(),
        "writing through symlink to external dir should be BLOCKED. Got: {:?}",
        result.ok()
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("outside") || err.contains("allowed"),
        "error should be about sandbox boundaries: {}",
        err
    );
}
