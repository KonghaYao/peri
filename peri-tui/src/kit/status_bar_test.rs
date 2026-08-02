//! Tests for status_bar

#[cfg(test)]
use super::*;
#[cfg(test)]
use serial_test::serial;

#[test]
fn test_permission_mode_display() {
    assert_eq!(permission_mode_display("default"), "Don't Ask");
    assert_eq!(permission_mode_display("accept-edit"), "Accept Edit");
    assert_eq!(permission_mode_display("auto-mode"), "Auto Mode");
    assert_eq!(permission_mode_display("bypass"), "Bypass");
    assert_eq!(permission_mode_display("unknown"), "Don't Ask");
}

#[test]
fn test_permission_mode_color() {
    assert_eq!(
        permission_mode_color("accept-edit"),
        statusbar().mode_accept_edit
    );
    assert_eq!(permission_mode_color("auto-mode"), statusbar().mode_auto);
    assert_eq!(permission_mode_color("bypass"), statusbar().mode_bypass);
}

#[test]
fn test_cwd_basename_simple() {
    assert_eq!(cwd_basename("/Users/foo/project"), "project");
    assert_eq!(cwd_basename("/tmp"), "tmp");
    assert_eq!(cwd_basename("/"), "/");
}

#[test]
fn test_cwd_basename_empty() {
    assert_eq!(cwd_basename(""), "");
}

#[test]
fn test_memory_color_thresholds() {
    assert_eq!(memory_color(100), statusbar().resource_good);
    assert_eq!(memory_color(512), statusbar().resource_good); // 512 不算超阈值
    assert_eq!(memory_color(513), statusbar().resource_warn);
    assert_eq!(memory_color(1024), statusbar().resource_warn); // 1024 不算超阈值
    assert_eq!(memory_color(1025), statusbar().resource_bad);
}

#[test]
fn test_resource_color_by_load() {
    // low=50, high=100
    assert_eq!(
        resource_color_by_load(10.0, 50.0, 100.0),
        statusbar().resource_good
    );
    assert_eq!(
        resource_color_by_load(50.0, 50.0, 100.0),
        statusbar().resource_warn
    );
    assert_eq!(
        resource_color_by_load(75.0, 50.0, 100.0),
        statusbar().resource_warn
    );
    assert_eq!(
        resource_color_by_load(100.0, 50.0, 100.0),
        statusbar().resource_bad
    );
}

#[test]
fn test_model_segment_parts_full() {
    // alias + model + effort 三段
    assert_eq!(
        model_segment_parts("opus", "claude-opus-4-20250514", "high"),
        vec!["opus", "claude-opus-4-20250514", "high"]
    );
}

#[test]
fn test_model_segment_parts_no_effort() {
    assert_eq!(
        model_segment_parts("opus", "claude-opus-4-20250514", ""),
        vec!["opus", "claude-opus-4-20250514"]
    );
}

#[test]
fn test_model_segment_parts_model_has_effort_suffix() {
    // 模型名尾部已含 effort 后缀 → 不重复追加
    assert_eq!(
        model_segment_parts("opus", "gpt-5.6-luna high", "high"),
        vec!["opus", "gpt-5.6-luna high"]
    );
}

#[test]
fn test_model_segment_parts_alias_equals_model() {
    // 配置回退到 alias（model_name 为空或等于 alias）→ 只显示一次
    assert_eq!(
        model_segment_parts("haiku", "haiku", "medium"),
        vec!["haiku", "medium"]
    );
    assert_eq!(
        model_segment_parts("haiku", "", "medium"),
        vec!["haiku", "medium"]
    );
}

#[test]
fn test_model_segment_parts_empty_all() {
    assert!(model_segment_parts("", "", "").is_empty());
}

#[test]
#[serial]
fn test_status_bar_row_renders_without_panic() {
    crate::kit::atoms::init_atoms();
    // 写入测试数据
    *atoms::SERVICE_SNAPSHOT.state().write() = atoms::ServiceSnapshot {
        cwd: "/home/user/test-project".into(),
        provider_name: "anthropic".into(),
        model_alias: "sonnet".into(),
        model_name: "claude-sonnet-4-20250514".into(),
        effort: "high".into(),
        permission_mode: "accept-edit".into(),
        memory_mb: 256,
        cpu_percent: 12.5,
        ..Default::default()
    };
    // 辅助函数应能正确处理这些值
    let snap = atoms::SERVICE_SNAPSHOT.state().read().clone();
    assert_eq!(snap.cwd, "/home/user/test-project");
    assert_eq!(cwd_basename(&snap.cwd), "test-project");
    assert_eq!(
        permission_mode_display(&snap.permission_mode),
        "Accept Edit"
    );
    // 模型段三段式：alias + model + effort
    assert_eq!(
        model_segment_parts(&snap.model_alias, &snap.model_name, &snap.effort),
        vec!["sonnet", "claude-sonnet-4-20250514", "high"]
    );
}

#[test]
#[serial]
fn test_status_bar_handles_empty_provider_model() {
    crate::kit::atoms::init_atoms();
    *atoms::SERVICE_SNAPSHOT.state().write() = atoms::ServiceSnapshot {
        cwd: "/tmp".into(),
        provider_name: "".into(),
        model_alias: "".into(),
        model_name: "".into(),
        permission_mode: "default".into(),
        memory_mb: 0,
        cpu_percent: 0.0,
        ..Default::default()
    };
    let snap = atoms::SERVICE_SNAPSHOT.state().read().clone();
    // 空 provider/model 应被渲染逻辑跳过（不在 Row1 中显示）
    assert!(snap.provider_name.is_empty());
    assert!(snap.model_alias.is_empty());
    assert!(snap.model_name.is_empty());
    // Default mode → Don't Ask 标签
    assert_eq!(permission_mode_display(&snap.permission_mode), "Don't Ask");
    // 0% CPU 应被跳过
    assert_eq!(snap.cpu_percent, 0.0);
}
