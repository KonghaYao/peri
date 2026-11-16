//! MCP 实例隔离契约测试（主 plan §6 D-03 / 验收契约 5，W5）。
//!
//! 这是**外部集成测试**（`tests/`，cargo 自动发现），因此只使用 `peri-middlewares`
//! 的 `pub` API：跳过 crate 内 readiness / system_tools seam（那些由 D-02 覆盖）。
//! fixture 沿用仓库既有 MCP stdio fixture 约定（`command: "node"` + 临时目录内脚本，
//! 见 `mcp/initialize_test.rs`），并在文件内自定义、不建共享 helper。
//!
//! # 断言范围（当前实现可观察的部分）
//!
//! - **独立 pool entry**：同一 pool 内每个 server name 一个 `clients` 条目，逐条目可见
//!   （`get_client` / `get_all_clients` / `all_server_infos` / `snapshot`）；
//! - **独立 `McpClientHandle`**：两台 server 的句柄是不同 `Arc`，各自携带自己那次
//!   `initialize` 的身份（`version` 来自各自 serverInfo）；
//! - **独立 transport wire**：两台 server 是两个真实子进程（各自独立 pid），每台只收到
//!   自己的 JSON-RPC 请求；
//! - **namespace 路由**：`mcp__{server}__{tool}` 只向所属 server 发 `tools/call`，
//!   wire 上使用的是该 server 的原始工具名；
//! - **无隐式跨 MCP 调用**：调用 A 不会在 B 的 wire 上产生任何请求，也不出现二次调用。
//!
//! 最后一条用例（`instances_have_independent_transport_task_and_state`，I01 / §8 第 15 行）
//! 走**另一条路径**：不做 `PERI_MCP_BUILTIN=off`，而是按生产装配步骤注入上下文后让真实
//! builtin `cron` 连上，断言其 transport / server task / 状态对象与 stdio server 互不共享，
//! 以及 close / reconnect / pool shutdown 的收敛面（与上面四条 stdio 用例**不同**，见其
//! 函数文档）。
//!
//! # PARTIAL —— 这些断言**不能**支撑「契约 5 完成」（主 plan §8 的 PARTIAL 分级）
//!
//! - **凭据隔离：未验证（UNVERIFIED）**。`McpClientHandle` 没有 credential 字段，
//!   凭证存储（`FileCredentialStore`）没有可安全读取的 per-instance identity。
//!   本文件不断言、也无从断言两台 server 的凭据不共享；不使用真实 secret，也不比较、
//!   打印任何凭据值。
//! - **capability root 隔离：未验证（UNVERIFIED）**。`McpClientPool::capability_profile`
//!   是 pool-wide 字段且非 public，`McpConnectionKey` 亦非 public，本文件无法读取或比较
//!   它们（因此也**未**断言 capability root 不共享）。
//! - **注册表内实例已实迁**。`web` / `artifact` / `cron` / `workspace` 均已
//!   落地为真实 builtin 实例。本文件的
//!   `instances_have_independent_transport_task_and_state` 走**真实 builtin cron**；
//!   其余四条仍只覆盖两台 stdio fixture 的「已落地连接局部隔离」。两者都不代表契约 5
//!   全文，也不代表契约 2/3/4（ready gate、direct 注入、空数组语义分别由 B-07 / D-02 负责）。
//!
//! # builtin 路径的可达面（I01 落点核查）
//!
//! 三个「handler 级」seam 是 `pub(crate)`，外部集成目标**不可达**：
//! `mcp::builtin::dispatch::builtin_server_handler`、
//! `mcp::builtin::runtime::spawn_builtin_transport_with_handler|with_context|with_tap`、
//! `mcp::client::McpClientPool::spawn_builtin_transport`（模块 `mcp::builtin` 自身是
//! `pub(crate) mod`，`mcp::client::transport` 是私有 `mod`）。
//!
//! 这**不构成缺口**：生产装配面本来就只经公开面注入 builtin 状态，因此真实 builtin
//! cron 在本文件里可完整驱动 —— `peri_middlewares::assembly::{BuiltinInstanceContext,
//! CronInstanceInput}` +
//! `McpClientPool::{set_builtin_instance_context, run_initialize, reconnect, set_disabled,
//! remove_server, shutdown}` 全部是 `pub`。本文件**未**为测试放宽任何生产符号可见性。

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use peri_agent::tools::{BaseTool, ToolContext};
use peri_mcp_common::process_env::{self, EnvLockFile};
use peri_middlewares::mcp;
use peri_middlewares::mcp::ClientStatus;
use peri_middlewares::mcp::McpClientHandle;
use peri_middlewares::mcp::McpClientPool;
use peri_middlewares::mcp::McpInitStatus;
use peri_middlewares::mcp::McpTaskOwner;
use peri_middlewares::mcp::McpToolBridge;
use serde_json::{json, Map, Value};

#[path = "mcp_isolation_contract/builtin.rs"]
mod builtin_contract;

#[path = "mcp_isolation_contract/test_cases.rs"]
mod test_cases;
use test_cases::EnvIsolation;
