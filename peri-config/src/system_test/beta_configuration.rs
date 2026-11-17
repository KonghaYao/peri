// ─── config.betas 投影与保存（Beta flag）────────────────────────────────────

/// 投影：workspace 覆盖 global，显式 false 关闭 global true；解释返回贡献者与规则。
#[test]
fn betas_projection_merges_layers_and_explains_sources() {
    use crate::betas::HISTORY_WINDOW_V1;
    use peri_acp_types::beta_flags::BetaFlagOrigin;
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let global = format!(r#"{{"config":{{"betas":{{"{HISTORY_WINDOW_V1}":true}}}}}}"#);
    let workspace = format!(r#"{{"config":{{"betas":{{"{HISTORY_WINDOW_V1}":false}}}}}}"#);
    let snapshot = ConfigurationSnapshot::resolve(
        scope.clone(),
        make_inputs(Some(&global), Some(&workspace), None),
    )
    .unwrap();
    assert!(
        !snapshot.flags().is_enabled(HISTORY_WINDOW_V1),
        "workspace 显式 false 关闭 global true"
    );
    assert_eq!(
        snapshot
            .flags()
            .value(HISTORY_WINDOW_V1)
            .map(|value| value.origin),
        Some(BetaFlagOrigin::Workspace)
    );

    let explanation = snapshot.explain(ConfigurationField::Betas);
    assert!(explanation
        .contributors
        .contains(&SourceIdentity::GlobalFile));
    assert!(explanation
        .contributors
        .contains(&SourceIdentity::WorkspaceFile));
    assert!(
        explanation.rule.contains("workspace wins"),
        "解释必须说明合并规则：{}",
        explanation.rule
    );

    // 未配置来源：一切按 false（快照缺失不意外开启能力）。
    let unset = ConfigurationSnapshot::resolve(
        make_scope(temp.path(), "other-project"),
        make_inputs(None, None, None),
    )
    .unwrap();
    assert!(unset.flags().is_empty());
    assert!(!unset.flags().is_enabled(HISTORY_WINDOW_V1));
}

/// 未知 flag 键被忽略且不影响其他配置领域。
#[test]
fn betas_unknown_keys_are_ignored_without_touching_other_domains() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let global = r#"{"config":{"language":"zh-CN","betas":{"not-a-flag":true}}}"#;
    let snapshot =
        ConfigurationSnapshot::resolve(scope, make_inputs(Some(global), None, None)).unwrap();
    assert!(snapshot.flags().is_empty(), "未知键不进入投影");
    assert_eq!(
        snapshot.settings().config.language.as_deref(),
        Some("zh-CN"),
        "未知键不影响同文件其他领域"
    );
    assert!(
        !snapshot
            .settings()
            .config
            .betas
            .overrides
            .contains_key("not-a-flag"),
        "未知键在解析后从内存配置剔除"
    );
}

/// 非 bool 值使该来源解析失败（不静默降级）。
#[test]
fn betas_non_bool_value_fails_that_source() {
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let global = r#"{"config":{"betas":{"history-window-v1":1}}}"#;
    let error = ConfigurationSnapshot::resolve(scope, make_inputs(Some(global), None, None))
        .expect_err("非 bool 值必须解析失败");
    assert!(
        matches!(
            error,
            ConfigurationError::InvalidInput {
                source_identity: SourceIdentity::GlobalFile,
                domain: "settings",
                ..
            }
        ),
        "错误必须定位到来源与领域：{error:?}"
    );
}

/// 保存往返：workspace 只写相对 global 的差异键，并经 CAS 发布新快照。
#[test]
fn betas_save_writes_workspace_diff_and_publishes() {
    use crate::betas::HISTORY_WINDOW_V1;
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_system(
        &scope,
        make_inputs(Some(r#"{"config":{}}"#), Some(r#"{"config":{}}"#), None),
    );
    let current = system.resolve(scope.clone()).unwrap();
    let mut updated: PeriConfig = current.settings().clone();
    updated.config.betas.set(HISTORY_WINDOW_V1, true);
    let next = system.update(&scope, current.revision(), &updated).unwrap();
    assert!(next.flags().is_enabled(HISTORY_WINDOW_V1));

    let writes = source.writes();
    assert_eq!(writes.len(), 1);
    let (path, expected, content) = &writes[0];
    assert_eq!(path, &scope.cwd.join(".peri/settings.json"));
    assert_eq!(expected.as_deref(), Some(r#"{"config":{}}"#));
    let document: Value = serde_json::from_str(content).unwrap();
    assert_eq!(
        document["config"]["betas"][HISTORY_WINDOW_V1],
        serde_json::json!(true)
    );
}

/// CAS 冲突不发布：revision 过期时拒绝写入并保留 current。
#[test]
fn betas_save_conflict_does_not_publish() {
    use crate::betas::HISTORY_WINDOW_V1;
    let temp = tempfile::tempdir().unwrap();
    let scope = make_scope(temp.path(), "project");
    let (source, system) = make_system(&scope, make_inputs(None, None, None));
    let stale = system.resolve(scope.clone()).unwrap();
    source.mutate_inputs(&scope, |inputs| {
        inputs.global = Some(r#"{"config":{"language":"en"}}"#.into())
    });
    let current = system.resolve(scope.clone()).unwrap();
    let mut updated: PeriConfig = current.settings().clone();
    updated.config.betas.set(HISTORY_WINDOW_V1, true);
    let error = system
        .update(&scope, stale.revision(), &updated)
        .unwrap_err();
    assert!(matches!(error, ConfigurationError::Conflict));
    assert!(source.writes().is_empty(), "冲突不写盘");
    assert!(Arc::ptr_eq(&current, &system.current(&scope).unwrap()));
    assert!(
        !system
            .current(&scope)
            .unwrap()
            .flags()
            .is_enabled(HISTORY_WINDOW_V1),
        "冲突不发布新投影"
    );
}
