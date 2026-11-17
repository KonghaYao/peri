# peri-genui：OpenUI Lang Rust 核心

## 目标与范围

1. 在 workspace 新增独立、同步、无框架依赖的 `peri-genui`；第二阶段接入 `peri-tui` 渲染 `gen-ui` 块。
2. 依据本地 `/tmp/openui-src/packages/lang-core`，OpenUI commit `1e24d7f5`，MIT；上游归属见 `peri-genui/NOTICE` 和 `LICENSE`。
3. 实现容错 lexer/AST/Pratt parser、语句切分和自动闭合、schema 编译、引用展开、校验、state 纯求值、动作描述及 `gen-ui` 扫描与提示词。
4. 内置 Count/First/Last/Sum/Avg/Min/Max/Sort/Filter/Round/Abs/Floor/Ceil/Each；动作描述 Action/Run/ToAssistant/OpenUrl/Set/Reset，支持 schema 声明的同步函数回调和自定义动作描述。

## 边界与拒绝项

1. Query/Mutation 仅解析分类与引用，求值对所有声明显式报告 `EvalError::Unsupported`，包括尚未被读取的声明；不使用默认数据假装已经执行。
2. 不实现 MCP、云服务、遥测、消息协议、网络请求、动作执行器、表单交互、编辑或 serialize 回语言；AST/错误的 serde 转换不等于语言 serializer。
3. 不引入 async、unsafe、ratatui、React 或 Zustand；所有直接依赖统一声明于 `[workspace.dependencies]`，crate 使用 `workspace = true`。
4. 不全局启用 `serde_json/preserve_order`；schema 从原始 JSON 直接反序列化为 `IndexMap`，组件参数及 `Element.props` 按声明顺序保存。

## API

| 入口 | Rust 契约 |
| --- | --- |
| `Library::from_json_str(&str)` | `Result<Library, LibraryError>`；支持 LibrarySpec 或裸 schema，保留 JSON 属性顺序 |
| `Library::from_value(Value, Option<String>)` | 编译 schema，可覆盖 root；无法恢复 `Value` 已丢失的原始 key 顺序 |
| `Library::root()` | `Option<&str>`；未声明 root 时仍可使用显式 `root = ...` |
| `parse(&str, &Library)` | 返回 `ParseResult`，只接收程序文本，不自动提取 Markdown |
| `StreamParser::new(Arc<Library>)` | `push(&str)`、`set(&str, complete: bool)`、`finish()` 返回 `ParseResult` |
| `scan_blocks(&str)` | 返回 `Vec<GenUiBlock>`：原文、程序、UTF-8 字节区间、关闭状态、开启行代码 |
| `system_prompt(&Library, &PromptOptions)` | 只介绍已实现能力；要求 `gen-ui` fence、开启后换行、root 首条 |
| `evaluate_root(&ParseResult, &EvalContext)` | 返回 `EvalResult { root, errors }`；state 覆盖不修改解析结果 |
| `ParseResult::to_fixture_json()` | 使用声明 state 默认值输出上游 fixture 形状；另有带 `EvalContext` 的版本 |

`ParseResult` 公开 root/errors/unresolved/orphans/incomplete/state/statements/root_expression；`Element` 公开 type_name/有序 props/partial/statement_id。`EvalContext` 提供 state、extra_scope、references、同步 `RuntimeFunction` 回调；回调错误保持其原始错误类型。

## 语义选择与已知差异

1. 规范歧义以 fixture 可观察行为优先；保留上游错误 code、组件归属、语句 ID 与字段路径，不以错误消息文本作为兼容契约。
2. `StreamParser` 重解析完整缓冲区；最终结果与 batch 一致。逐字符输入累计成本为二次量级，不移植上游增量缓存，也不保证每个中间快照一致。
3. `scan_blocks` 只识别 `gen-ui`，支持反引号/波浪号 fence 和未关闭块；保留开启行代码便于诊断，但 prompt 禁止这种输出。runner 单独归一化上游 fence 名称。
4. Sort 使用 ICU 固定 root collation，不依赖宿主 locale；混合类型比较沿用上游规则，但不保证非传递比较下与任意 JS 引擎得到同一排列。
5. schema 校验实现上游子集，不宣称完整 JSON Schema 支持；复合 schema 对普通数据保留上游宽容行为，缺失/类型/枚举/默认值/数组项裁剪单独校验。组件 slot 按 `type_name` 匹配；枚举对所有 JSON 值精确比较且不跳过声明类型；求值阶段对组件属性再校验类型/required/default，`Binding` 跳过。
6. binding 标记来自 LibrarySpec 的 `$binding<...>` 组件签名；裸 schema 没有该标记，不能据此推断双向绑定。
7. 内部类型区分 Expr、ActionPlan、Binding 和普通 JSON；普通数据不能伪造自定义动作身份。公开 JSON 只作数据，不能作为宿主执行授权。
8. 输入递归、token 和引用/AST 展开受预算限制；action schema 展开共享预算 4096 次，生成值单值 4 MiB、state 默认值总量 16 MiB；超限显式报错。Query 依赖环的 `query-cycle` 专用诊断未移植，仍由 unsupported 阻止执行。
9. 同步宿主回调的 panic、耗时和宿主自行构造的极深 `Value` 不属于模型文本解析器的隔离保证；本阶段不引入线程或沙箱。

## TUI 接入（第二阶段）

1. `markdown/parser.rs` 在 Markdown 解析前用 `scan_blocks` 提取 `gen-ui` 围栏，将块内三反引号替换为占位符并规范化为 `gen-ui`/`gen-ui-partial`；`convert.rs` 还原后调用 `parse`，产出 `MarkdownSegment::GenUi`。
2. `boundary.rs` 在 `gen-ui` opener 处停止 stable chunk，块留在可变 tail；首版不做块级稳定缓存，避免复用时序问题。
3. 渲染器位于 `peri-tui/src/kit/genui/`，首版组件为 Stack、Card、Text、Heading、List、Table、KeyValue、Progress、Divider、Button；后续补充 BarChart、Sparkline。Text 按纯文本换行，不递归 Markdown。
4. Button 支持整行点击；`@ToAssistant` 回填输入区，`@Set`/`@Reset` 修改块级视图状态并重渲染，`@Run`/`@OpenUrl` 显示未支持提示。视图状态不进 transcript，相同源码和入口的重复块共享状态；流式未闭合块期间 Set/Reset 禁用并提示，闭合后恢复。
5. 尚未把 `system_prompt` 注入 Agent 系统提示词；模型当前需要由宿主另行告知组件库与 `gen-ui` 围栏约定。SDK 的 32 组件 OpenUI 库与当前 12 组件库是两套定义，后续需要收敛或显式区分。

## 验收与测试口径

1. 不复制上游 fixture 用例；`peri-genui/tests/library.json` 仅作为组件库输入，原文来自上游同一 commit。
2. 核心回归覆盖参数顺序、校验与裁剪、状态覆盖、所有内置函数、动作身份、错误透传、unsupported、扫描器、提示词及畸形/深层/循环输入；crate 文档示例可运行。
3. 执行 `cargo test -p peri-genui`、`cargo clippy -p peri-genui --all-targets -- -D warnings`、`cargo fmt --all`；workspace 依赖需要既有补丁时使用 `scripts/cargo-rmcp-patched.sh` 的等价 test/clippy 命令，不修改其他 crate 来绕过环境问题。
4. TUI 侧执行 `cargo test -p peri-tui --lib`、`cargo clippy -p peri-genui -p peri-tui --all-targets -- -D warnings`、`cargo fmt --all --check`、`bash scripts/check-changed.sh`。`cargo test` 全量并行曾观察到一次既有 `FOLD_OVERRIDES` 全局状态竞争，重跑通过；该用例与新 crate 无关。

## 本次验证结果

1. `./scripts/cargo-rmcp-patched.sh test -p peri-genui --locked -- --nocapture`：核心回归 29 通过、unit 1 通过、doc test 1 通过，失败 0。
2. `./scripts/cargo-rmcp-patched.sh clippy -p peri-genui --all-targets --locked -- -D warnings`、显式 `test -p peri-genui --doc --locked`、`cargo fmt --all` 及格式复检通过；新增 Rust 文件均小于 1000 行。
3. 不带补丁脚本的 test/clippy 因 workspace 现有 `turso_serverless 0.1.3` 缺少 `wasm-fetch` feature 而退出 101；没有修改其他依赖来绕过冲突。
4. 验证环境为 `rustc 1.99.0`，manifest 继承 workspace 的 Rust 1.95 要求；未单独用 1.95 编译。锁文件只保留新 crate 及其依赖所需增量，未保留顺带更新的 libc/windows-sys。
