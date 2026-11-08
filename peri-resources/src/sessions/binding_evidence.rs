//! 绑定行的创建证据：完整发现快照 → 最小执行根。
//!
//! `session_bindings.discovery_snapshot` 只有两类读取方：远端 `observe_binding` 取 `root`
//! 定位执行目录与反查归属行，virtual 分支另外取 `kind` 判别布局。文件对象身份随环境变化、
//! 跨主机不可通用，且没有读取方；Git 布局只服务于诊断（完整布局仍留在
//! `workspaces.discovery`）。因此入库值收缩为：
//!
//! - native：`{"root":"<绝对路径>"}`
//! - virtual：`{"kind":"virtual-v1","root":"<绝对路径>"}`
//!
//! 投影是幂等的：对已收缩的形态再投影结果不变，因此迁移、新建与 fork 自复制三条路径
//! 可以共用同一份判定，不会出现双形态并存。

use anyhow::{bail, Result};

/// 把创建证据收缩为最小执行根。输入可以是完整 `Discovery`、已收缩形态或两者的混合。
///
/// - 解析失败、`root` 缺失/非字符串/空 → `Err`（调用方决定 fail-closed 还是拒绝输入）；
/// - `root` 为相对路径 → `Err`；判定用 `has_root` 而不是平台相关的 `is_absolute`，
///   因为 `/work` 在 Windows 上仍由根分量锚定、不会随进程 CWD 漂移（真实发现值由
///   `canonicalize` 产生，永远带盘符；该判据同时允许跨平台旧库与测试夹具）；
/// - `kind` 存在且为字符串时保留，其余类型直接丢弃（它不参与任何判定）。
pub(super) fn project(raw: &str) -> Result<String> {
    let value: serde_json::Value = serde_json::from_str(raw)?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("binding evidence is not an object"))?;
    let root = object
        .get("root")
        .and_then(serde_json::Value::as_str)
        .filter(|root| !root.is_empty())
        .ok_or_else(|| anyhow::anyhow!("binding evidence has no root"))?;
    if !std::path::Path::new(root).has_root() {
        bail!("binding evidence root is not rooted");
    }
    let projected = match object.get("kind").and_then(serde_json::Value::as_str) {
        Some(kind) => serde_json::json!({ "kind": kind, "root": root }),
        None => serde_json::json!({ "root": root }),
    };
    Ok(serde_json::to_string(&projected)?)
}

#[cfg(test)]
mod tests {
    use super::project;

    #[test]
    fn full_discovery_projects_to_its_root() {
        let raw = r#"{"root":"/work","root_identity":{"device":1,"inode":2},"common_dir":"/work/.git","common_identity":{"device":1,"inode":3},"private_dir":"/work/.git","private_identity":{"device":1,"inode":4}}"#;
        assert_eq!(project(raw).unwrap(), r#"{"root":"/work"}"#);
    }

    #[test]
    fn already_minimal_native_is_unchanged() {
        assert_eq!(
            project(r#"{"root":"/work"}"#).unwrap(),
            r#"{"root":"/work"}"#
        );
    }

    #[test]
    fn virtual_keeps_kind() {
        let raw = r#"{"kind":"virtual-v1","root":"/work","root_identity":{"device":1,"inode":2}}"#;
        assert_eq!(
            project(raw).unwrap(),
            r#"{"kind":"virtual-v1","root":"/work"}"#
        );
    }

    #[test]
    fn projection_is_idempotent() {
        for raw in [
            r#"{"root":"/work"}"#,
            r#"{"kind":"virtual-v1","root":"/work"}"#,
            r#"{"root":"/work","common_dir":"/work/.git","private_dir":"/work/.git"}"#,
        ] {
            let once = project(raw).unwrap();
            assert_eq!(project(&once).unwrap(), once);
        }
    }

    #[test]
    fn missing_root_is_rejected() {
        assert!(project(r#"{"common_dir":"/work/.git"}"#).is_err());
        assert!(project(r#"{"root":null}"#).is_err());
        assert!(project(r#"{"root":""}"#).is_err());
        assert!(project(r#"{"root":7}"#).is_err());
        assert!(project(r#"{"root":"relative/path"}"#).is_err());
        assert!(project(r#"{"root":"C:work"}"#).is_err());
        assert!(project("[]").is_err());
        assert!(project("not json").is_err());
    }

    #[test]
    fn non_string_kind_is_dropped_without_blocking() {
        assert_eq!(
            project(r#"{"kind":7,"root":"/work"}"#).unwrap(),
            r#"{"root":"/work"}"#
        );
    }
}
