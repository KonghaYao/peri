# Micro Compact 后模型无法读取本轮用户图片

**状态**：Fixed（运行中追加图片缺口已修复，现场对应关系待确认）
**优先级**：高
**创建日期**：2026-09-20

## 问题描述

用户发送图片并要求「按照图中的数据修改」，紧接着发生 Micro Compact，模型回复无法读取截图中的文字和数值，要求重新上传图片或粘贴数据。期望压缩后仍能读取本轮用户图片并继续任务。

## 症状详情

- 截图中附件为 `01a0bdae-5da0-7d61-af98-0f173a9003a1.png`，321.0 KB。
- 提示 `Micro compaction completed (10 messages, ~4993 tokens saved, 0 files, 0 skills)`。
- 随后模型回复「我目前无法读取这张截图中的文字和数值」。
- 目前为用户观察，尚未确定图片在哪一层丢失，也不能仅凭回复断定原始附件被删除。

## 复现条件

- **复现频率**：未知，用户提供一次现场截图。
- **触发步骤**：长会话中发送图片及修改要求；自动触发 Micro Compact；模型回答无法读取图片。
- **环境**：Peri TUI，具体运行版本和模型未知。

## 涉及文件

- `peri-agent/src/agent/stages/mod.rs` —— 每批 Receive 后的输入准备调用。
- `peri-agent/src/agent/stages/middleware_runner.rs` —— 后续批次转换与错误回写。
- `peri-agent/src/middleware/{capabilities,trait,chain}.rs` —— 输入准备能力、hook 与首批顺序。
- `peri-middlewares/src/middleware/image/{mod,mod_test}.rs` —— 图片转换与完整循环回归。

## 调查证据与局限

- 只读核对现场 thread `01a0bcda-df74-70e1-9eaa-34d2e801e1a6`：消息 `01a0bdae-d284-7be0-be32-bd0bf2b5f97b` 的持久化内容为 `@image` 路径及实际换行后的指令，`truncated=false`、`excluded=false`、`projection=NULL`；对应原图仍存在，328722 bytes。
- 存储中的纯文本不能单独证明 live 请求未转换：`MessageTranscript::replace_by_id` 只更新内存。未取得现场原始模型请求及同一 run 的输入批次边界，暂不把现场归因写成已证实。
- 当前 Micro planner 仅规划旧工具结果，不规划用户图片。确认的代码缺口是 `before_agent_has_run` 让 ImageMiddleware 仅处理首次 Receive；同一 run 的后续图片输入跳过附件转换。
- 新回归在修复前失败于「每批输入都必须向模型传递图片字节」，修复后通过，并确认实际发生 Micro、首批及后续输入图片载荷保留。

## 状态变更记录

| 日期 | 从 | 到 | 操作人 | 说明 |
|------|-----|-----|--------|------|
| 2026-09-20 | — | Open | agent | 根据用户截图创建 |
| 2026-09-20 | Open | Fixed | agent | 修复同一 run 后续图片输入跳过准备的问题；现场归因待用户确认 |

## 修复记录

### 修复 #1（2026-09-20）

- **操作人**：agent
- **用户原意**：发送的图片在 compact 后仍应可供模型读取。
- **修复内容**：新增窄能力 `BeforeInputState` 和逐批 `before_input`；ImageMiddleware 迁入该 hook。首次按原中间件顺序交错初始化与输入准备，后续批次只准备输入；空批次跳过，错误前完成的转换仍回写。后续 hook 失败停止本轮，中断保持 Interrupted 语义；首次初始化错误沿用原策略。
- **验证**：`cargo test -p peri-middlewares --lib middleware::image`（8 通过）；`cargo test -p peri-agent --lib middleware`（39 通过）；`cargo build --workspace`（exit 0，链接器报告 debug unwind section 大小警告）。`cargo test -p peri-agent --doc`（8 通过）。`cargo test -p peri-middlewares --doc` 最终 exit 0（唯一示例 ignored，未执行文档用例）；前两次遇到缺失/旧编译产物，刷新源文件时间戳重建后通过编译。`rustfmt --check`（仅本次 Rust 文件）与 `git diff --check` 均通过。
- **验证状态**：代码回归通过，现场待验证；未覆盖真实 provider 请求和现场运行版本。未修改原会话或图片。
