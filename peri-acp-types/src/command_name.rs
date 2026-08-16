//! 命令全名词法契约（设计文档「权威词法」§42-59）。
//!
//! 统一字符串结构：`裸名` / `domain:name`（第一等级）/ `domain:namespace:name`
//! （第二等级）。层数上限 2 段冒号（3 段词），最右冒号切分（与
//! `mcp_skills.rs` / `skill_preload.rs` 先例一致）；段统一小写归一，
//! 小写全名 = 唯一键；`mcp__` 双下划线遗留形态解析即失败。
//!
//! 词法结构直接携带路由信息：domain = 来源域（provenance 首段），
//! namespace = 来源域内标识（server / 插件名），name = 命令名。
//! 解析仅做词法判定，不执行路由；裸名 → core/ui 域展开属注册表层。

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

/// 第一等级域（可裸名 / 1 层显式）：`core`（内置命令 + 本地 skill）、
/// `ui`（TUI 面板）。本地 skill 归第一层级，`/skill-name` 短输入体验保留。
///
/// 同步约束（P2-1 审查跟进）：本域集合与 `command_route.rs::CommandSource`
/// 变体一一对应（Core/Ui ↔ 第一等级，Mcp/Plugin/User ↔ 第二等级）——
/// 新增来源域需两处同步（本文件常量 + `CommandSource` 变体），漏一处则
/// "解析接受但路由无此域"或反之，且无编译期提示。单点化（`CommandSource`
/// 提供 `domains()` 供本常量引用）留 Phase 2 注册表接入时执行，避免词法层
/// 反向依赖路由层。
const LEVEL1_DOMAINS: [&str; 2] = ["core", "ui"];

/// 第二等级域（外部来源，必须完整 2 层形态）：`mcp` / `plugin` / `user`。
/// namespace 显式标记不可省略——`mcp:demo:hello`、`plugin:ecc:deploy`；
/// `mcp:hello` 形态对外部来源非法。
///
/// 同步约束同 [`LEVEL1_DOMAINS`]：与 `command_route.rs::CommandSource`
/// 变体一一对应，新增域需两处同步（见上）。
const LEVEL2_DOMAINS: [&str; 3] = ["mcp", "plugin", "user"];

/// 命令名词法形态（设计文档「权威词法」§44-59）。
///
/// 段统一小写归一（与 alias 小写索引同源）；`__` 双下划线形态解析即失败。
/// 非 wire 契约成员（计划步骤 2 代码形态：仅 [`CommandLevel`] 带 serde；
/// 本类型为内存形态，Phase 3 投影序列化勿直接导出）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandName {
    /// 裸名（无冒号）：第一等级域（core / ui）快捷匹配，**非唯一键**
    /// （裸名由解析层在 core/ui 域内精确匹配展开）。
    Bare { name: String },
    /// 第一等级显式：`domain:name`（core:compact / ui:history）。
    Level1 { domain: String, name: String },
    /// 第二等级完整：`domain:namespace:name`（mcp:demo:hello）。
    Level2 {
        domain: String,
        namespace: String,
        name: String,
    },
}

/// 词法等级（第一等级可裸名 / 1 层显式；第二等级必须完整 2 层形态）。
///
/// serde 输出为 externally tagged 字符串 `"Level1"` / `"Level2"`，与设计
/// §85 投影 `level（1 | 2）` 的数字语义不一致——Phase 3 投影序列化需手工
/// 映射为数字或统一 wire 形态，勿直接导出本类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandLevel {
    /// 第一等级：core / ui。
    Level1,
    /// 第二等级：mcp / plugin / user。
    Level2,
}

/// 词法解析错误。`mcp__` 双下划线形态（含 mcp__s__k）属废弃词法，解析即失败。
/// 非 wire 契约成员（计划步骤 2 代码形态无 serde；错误仅消费方本地展示）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CommandNameError {
    /// 命令名为空。
    #[error("命令名为空")]
    Empty,
    /// 冒号段数超过上限（最多 2 段冒号 / 3 段词）。
    #[error("命令名冒号段数超过上限（最多 2 段冒号 / 3 段词）")]
    TooManySegments,
    /// 含空段（开头/结尾/连续冒号）。
    #[error("命令名含空段（开头/结尾/连续冒号）")]
    EmptySegment,
    /// 含 `__` 双下划线遗留形态（含 mcp__ 前缀形态）。
    #[error("命令名含 `__` 双下划线形态（已废弃，含 mcp__ 遗留形态）")]
    DoubleUnderscore,
    /// 段含空白字符（防与 args 分离混淆）。
    #[error("命令名段含非法字符: {0:?}")]
    IllegalCharacter(char),
    /// 显式形态的域不在该等级的声明集合内（第一等级 core/ui；第二等级
    /// mcp/plugin/user）。
    #[error("命令域 `{0}` 在该词法形态下不合法（第一等级: core/ui；第二等级: mcp/plugin/user）")]
    UnknownDomain(String),
    /// 第二等级域缺 namespace 的 1 层形态（`mcp:hello` 对外部来源非法，
    /// 设计 §54：namespace 显式标记不可省略——`mcp:demo:hello`）。
    #[error("第二等级域 `{domain}` 必须带 namespace（如 `{domain}:server:name`）")]
    MissingNamespace { domain: String },
}

impl CommandName {
    /// 词法解析：最右冒号切分（rsplit_once 语义），段数 0/1/2 →
    /// Bare / Level1 / Level2，>2 拒绝（设计 §52 层数上限）。
    ///
    /// 输入约定：不含 `/` 前缀、不含 args（args 分离属 Phase 2 CommandParse）。
    ///
    /// 解析规则（对应 Phase 1 计划步骤 2 验收标准）：
    /// ① 空输入 → `Empty`；② 任何段含 `__` → `DoubleUnderscore`
    /// （覆盖 mcp__s__k / mcp__demo__hello 全部遗留形态，规则取"段含双下划线
    /// 即非法"而非仅 mcp 前缀，同类 plugin__x__y 一并拒绝）；
    /// ③ 冒号段计数 0/1/2 → 对应变体，>2 → `TooManySegments`；
    /// ④ 空段（`:a` / `a:` / `a::b`）→ `EmptySegment`；
    /// ⑤ 段含空白字符 → `IllegalCharacter`（防与 args 分离混淆）；
    /// ⑥ 显式形态域不在该等级声明集合 → `UnknownDomain`；第二等级域缺
    /// namespace 的 1 层形态（`mcp:hello`）→ `MissingNamespace`；
    /// ⑦ 段统一 `to_lowercase()` 归一。
    pub fn parse(input: &str) -> Result<CommandName, CommandNameError> {
        if input.is_empty() {
            return Err(CommandNameError::Empty);
        }
        // 双下划线遗留形态整体拒绝（冒号与下划线不重叠，整体 contains 等价于
        // 逐段检查）。
        if input.contains("__") {
            return Err(CommandNameError::DoubleUnderscore);
        }
        let segments: Vec<&str> = input.split(':').collect();
        if segments.len() > 3 {
            return Err(CommandNameError::TooManySegments);
        }
        if segments.iter().any(|s| s.is_empty()) {
            return Err(CommandNameError::EmptySegment);
        }
        // 段含空白字符 → 非法（防与 args 分离混淆）。
        for seg in &segments {
            if let Some(c) = seg.chars().find(|c| c.is_whitespace()) {
                return Err(CommandNameError::IllegalCharacter(c));
            }
        }
        // 段统一小写归一（唯一键 = 全名小写）。
        let lower: Vec<String> = segments.into_iter().map(|s| s.to_lowercase()).collect();
        match lower.len() {
            1 => {
                let name = lower.into_iter().next().expect("len == 1");
                Ok(CommandName::Bare { name })
            }
            2 => {
                let mut it = lower.into_iter();
                let domain = it.next().expect("len == 2");
                let name = it.next().expect("len == 2");
                if LEVEL2_DOMAINS.contains(&domain.as_str()) {
                    // 第二等级域缺 namespace 的 1 层形态（mcp:hello）对
                    // 外部来源非法（设计 §54）。
                    return Err(CommandNameError::MissingNamespace { domain });
                }
                if !LEVEL1_DOMAINS.contains(&domain.as_str()) {
                    return Err(CommandNameError::UnknownDomain(domain));
                }
                Ok(CommandName::Level1 { domain, name })
            }
            3 => {
                let mut it = lower.into_iter();
                let domain = it.next().expect("len == 3");
                let namespace = it.next().expect("len == 3");
                let name = it.next().expect("len == 3");
                if !LEVEL2_DOMAINS.contains(&domain.as_str()) {
                    return Err(CommandNameError::UnknownDomain(domain));
                }
                Ok(CommandName::Level2 {
                    domain,
                    namespace,
                    name,
                })
            }
            _ => unreachable!("段数 0/1/2/3 已由上文约束"),
        }
    }

    /// 末段命令名（Bare / Level1 / Level2 的 name 字段）。
    pub fn name(&self) -> &str {
        match self {
            CommandName::Bare { name } => name,
            CommandName::Level1 { name, .. } => name,
            CommandName::Level2 { name, .. } => name,
        }
    }

    /// 小写规范全名 = 唯一键（Level1: `domain:name`；Level2:
    /// `domain:namespace:name`；Bare: 返回裸名小写，非键，路由层按 core/ui
    /// 展开）。输出前各段再归一一次，直接构造大写字段也保证键不变。
    pub fn full_name(&self) -> String {
        match self {
            CommandName::Bare { name } => name.to_lowercase(),
            CommandName::Level1 { domain, name } => {
                format!("{}:{}", domain.to_lowercase(), name.to_lowercase())
            }
            CommandName::Level2 {
                domain,
                namespace,
                name,
            } => format!(
                "{}:{}:{}",
                domain.to_lowercase(),
                namespace.to_lowercase(),
                name.to_lowercase()
            ),
        }
    }

    /// 裸名 → 第一等级域展开候选（core/ui 两键），供路由层单一调用点使用
    /// （设计 §86：裸名由解析层在 core/ui 域内精确匹配展开）；非裸名形态
    /// 返回 `None`。展开规则固化于此，避免散落 Phase 2 注册表实现。
    ///
    /// ⚠️ Phase 2 注册表已采用 alias_index 裸名登记（第一等级裸名 → fullname
    /// 单键映射，跨域同名互斥注册），本 API 仅供词法层推导使用，勿在
    /// 路由/解析路径调用（避免「解析唯一实现」不变式 3 之外的第二套语义）。
    pub fn bare_level1_keys(&self) -> Option<[String; 2]> {
        match self {
            CommandName::Bare { name } => {
                let name = name.to_lowercase();
                Some([format!("core:{name}"), format!("ui:{name}")])
            }
            _ => None,
        }
    }

    /// 词法等级：Bare / Level1 → Level1；Level2 → Level2。
    pub fn level(&self) -> CommandLevel {
        match self {
            CommandName::Bare { .. } | CommandName::Level1 { .. } => CommandLevel::Level1,
            CommandName::Level2 { .. } => CommandLevel::Level2,
        }
    }
}

impl fmt::Display for CommandName {
    /// 全名小写规范化输出（唯一键形态；Bare 输出裸名小写）。注意：Level1
    /// 输出 `core:compact` 而非 UI 展示形态 `compact`（设计 §87）——Phase 3
    /// 投影渲染勿直接复用本 Display。
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.full_name())
    }
}

impl FromStr for CommandName {
    type Err = CommandNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        CommandName::parse(s)
    }
}

#[cfg(test)]
#[path = "command_name_test.rs"]
mod tests;
