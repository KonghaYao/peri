# Read 工具应优雅处理目录输入，folder_operations 需添加 deep_scan 递归树功能

**状态**：Fixed
**优先级**：中
**类型**：Bug + 功能增强
**创建日期**：2026-07-20

## 问题描述

当前 Read 工具在用户传入文件夹路径时，直接抛出 `Is a directory (os error 21)` 底层 I/O 错误，对 LLM 不友好。合理的做法是识别到这是一个目录后，自动列出目录内容并提示用户。同时，folder_operations 的 `list` 操作只能列出单层内容，缺少递归获取多层文件树的能力，需要一个 `deep_scan` 操作（或参数）来按 `max_depth` 控制展开层级。

## 症状详情

### 子问题 1：Read 工具读取目录时行为不当

| 项 | 内容 |
|---|------|
| **当前行为** | `Read("/path/to/folder")` → `Error: Is a directory (os error 21)` |
| **期望行为** | 检测到路径是目录后，返回目录内文件列表（类似 `folder_operations list`），并在输出中显式提示"这是一个目录，以下是目录内容" |
| **触发方式** | 任何 Read 调用传入的是文件夹路径而非文件路径时必现 |
| **影响** | LLM 收到的错误信息对后续决策没有帮助，需要额外工具调用（folder_operations）才能获取信息 |

### 子问题 2：folder_operations 缺少递归文件树能力

| 项 | 内容 |
|---|------|
| **当前行为** | `folder_operations list` 仅返回指定路径的直接子文件和子目录（单层） |
| **期望行为** | 支持 `deep_scan` 操作（或 `list` 操作的可选 `deep_scan` 参数），返回从指定路径开始的递归文件树。通过 `max_depth` 参数控制展开层级（1=仅当前层，2=包含一级子目录，3=包含两级子目录，以此类推） |
| **触发方式** | 需要了解目录结构全貌（而非仅内容列表）时 |

## 涉及文件

- `peri-middlewares/src/tools/filesystem/read.rs:162-181` —— Read 工具的 `invoke` 方法，在第 162 行通过 `std::fs::read_to_string` 读取文件，未提前判断路径是否为目录
- `peri-middlewares/src/tools/filesystem/folder.rs:41-130` —— `list_folder` 函数及 FolderOperationsTool 的 `parameters()` 和 `invoke()` 方法，当前仅支持单层列表

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-07-20 | — | Open | agent | 创建 |

## 修复记录

（由 fix-issue 或 issue-verify skill 追加，创建时留空）
