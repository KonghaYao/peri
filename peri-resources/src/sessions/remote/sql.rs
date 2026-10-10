//! 参数化语句与取值解码的共用原语。
//!
//! 远程路径只有一种语句形态：**静态 SQL + 全部值走绑定参数**。SQL 文本是 `&'static str`，
//! 动态内容（会话 id、store id、收据、时间戳）只能出现在 `params` 里；表名等无法绑定的
//! 位置不得由外部输入拼出。这条约束由 `StatementSpec` 的形状和离线测试共同保证。

use std::fmt;

use turso_serverless::Value;

/// 一条参数化语句。
///
/// `sql` 是静态文本（不含插值），`params` 只按位置绑定；无参数语句用空 `Vec`。
#[derive(Clone, PartialEq)]
pub(super) struct StatementSpec {
    pub(super) sql: &'static str,
    pub(super) params: Vec<Value>,
}

impl fmt::Debug for StatementSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StatementSpec")
            .field("sql", &self.sql)
            .field("params", &self.params)
            .finish()
    }
}

impl StatementSpec {
    pub(super) fn new(sql: &'static str, params: Vec<Value>) -> Self {
        Self { sql, params }
    }

    pub(super) fn bare(sql: &'static str) -> Self {
        Self {
            sql,
            params: Vec::new(),
        }
    }

    /// 该语句是否只读（只用于离线断言：读路径不得出现写入语句）。
    #[cfg(test)]
    pub(super) fn is_read_only(&self) -> bool {
        let head = self.sql.trim_start().to_ascii_uppercase();
        head.starts_with("SELECT") || head.starts_with("WITH")
    }
}

/// 第 `index` 列是 TEXT 时返回其值。
pub(super) fn text_at(values: &[Value], index: usize) -> Option<&str> {
    match values.get(index) {
        Some(Value::Text(text)) => Some(text.as_str()),
        _ => None,
    }
}

/// 第 `index` 列是 INTEGER 时返回其值。
pub(super) fn int_at(values: &[Value], index: usize) -> Option<i64> {
    match values.get(index) {
        Some(Value::Integer(value)) => Some(*value),
        _ => None,
    }
}
