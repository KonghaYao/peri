# peri-theme 代码索引

> 速查表：把「我想做什么」映射到文件。细节以代码为准。更新：2026-09-10。
> 依据：`peri-theme/Cargo.toml`、源码、契约测试与 `docs/standards/tui.md`；本 crate 无独立 CLAUDE.md。

## 架构速览

`peri-theme` 提供主题类型、默认主题、JSON 加载和 TUI 配色投影；不依赖 Agent、ACP 或 TUI。
数据流是来源查找 → 继承叶子合并 → 引用解析 → typed token 构造 → bridge → atoms。
`loader.rs` 保留公共 API 和错误类型，三个私有模块分别管理来源、纯解析和类型构造。

`themes/{dark,light}.json` 是常规加载的内置定义；`builtin.rs` 提供同步 Rust 默认构造，
供 atom 初始化及旧主题缺省字段使用。完整主题相等测试约束这两份定义保持一致。
`src/lib.rs::prelude` 只 re-export 各模块的公共类型和入口，不另设主题状态。

## 速查表

| 我想做什么 | 主文件 | 入口/关键函数 | 关键逻辑 |
| --- | --- | --- | --- |
| 加载主题 | `peri-theme/src/loader.rs` | `load_theme`:38、`ThemeLoadError`:17 | 返回 `Arc<ThemeDefinition>`；用户文件优先于内置 JSON，已有用户文件解析失败直接返回错误 |
| 查看主题名称 | `peri-theme/src/loader.rs` | `list_available_themes`:45 | 委托来源目录枚举，排序去重，不预先解析 JSON |
| 调整 HOME 和内置别名查找 | `peri-theme/src/loader/source.rs` | `ThemeSources::from_environment`:16、`read`:79 | 读取 `~/.peri/themes/{name}.json`，再查 dark/peri-dark、light/peri-light；根加载保留路径和符号链接支持 |
| 修改继承策略 | `peri-theme/src/loader/source.rs` | `load_flat`:28、`merge_document`:46、`validate_name`:122 | `extends` 只接受主题名；当前链检测循环，最多 10 个父级；父先子后覆盖叶子键，子 name 默认 unnamed、mode 可继承 |
| 支持更多 JSON 表达形式 | `peri-theme/src/loader/resolve.rs` | `flatten_json_obj`:12 | 对象用点连接，数组用数字索引；保留键大小写，字符串、数字、布尔转为文本 |
| 修改引用规则 | `peri-theme/src/loader/resolve.rs` | `resolve_refs`:47、`resolve_ref_value`:69 | 每个根独立活动链；先精确键查找再不区分大小写匹配；最多 10 条引用边，未解析键返回错误 |
| 修改颜色格式 | `peri-theme/src/loader/resolve.rs` | `parse_hex_color`:103 | trim 后仅接受 ASCII #RGB/#RRGGBB；先验证字节再切片，Unicode 非法输入返回错误 |
| 添加或调整 token 解码 | `peri-theme/src/loader/decode.rs` | `decode_flat`:11、`build_theme_from_flat`:50 | 提取元数据、解析全部引用后构造 Palette/SemanticTokens/ComponentTokens，必填项缺失报错 |
| 兼容旧版用户主题字段 | `peri-theme/src/loader/decode.rs` | `build_session_title_palette`:32、`build_theme_from_flat`:50 | 新语义键和会话标题色板保留既有默认值；色板逐项允许缺省或非法值回退 |
| 修改默认主题 | `peri-theme/src/builtin.rs`、`peri-theme/themes/dark.json`、`peri-theme/themes/light.json` | `dark_theme`:82、`light_theme`:239 | Rust 默认与内置 JSON 的整个 ThemeDefinition 由相等测试约束，light 主文字为 #1E1E1E |
| 将主题映射到 ratatui-kit | `peri-theme/src/bridge.rs` | `ThemeDefinitionExt`:11、`to_palette`:19 | 映射到 kit 的 Palette，保持上游类型边界 |
| 将主题映射到 Peri 颜色 | `peri-theme/src/bridge.rs` | `to_peri_colors`:39、`default_peri_colors`:81 | 从语义 token 派生 PeriColors，默认构造使用 dark |
| 切换全局主题 | `peri-theme/src/atoms.rs` | `init_theme_atoms`:27 | 先派生 Palette/PeriColors，再依次写 THEME_ATOM、PALETTE_ATOM、PERI_COLORS_ATOM |

## 类型与消费边界

| 功能 | 文件 | 入口/关键点 |
| --- | --- | --- |
| 主题身份与模式 | `peri-theme/src/theme.rs` | `ThemeMode`:13、`ThemeDefinition`:21；可克隆、比较和序列化 |
| 基础色板 | `peri-theme/src/palette.rs` | `Palette`:11；crate 自有类型，区别于 bridge 输出的 kit Palette |
| 语义色 | `peri-theme/src/semantic.rs` | `SemanticTokens`:11、`AccentTokens`:40、`SyntaxTokens`:55 |
| 组件 token | `peri-theme/src/component.rs` | `ComponentTokens`:12；message/input/panel/popup/statusbar/markdown/scrollbar |
| 非组件配色投影 | `peri-theme/src/peri_colors.rs` | `PeriColors`:11；`Default` 使用 Color::Reset，不读取全局 atom |
| 完整主题订阅 | `peri-theme/src/atoms.rs` | `THEME_ATOM`:18；Arc 包装的 ThemeDefinition |
| kit 配色订阅 | `peri-theme/src/atoms.rs` | `PALETTE_ATOM`:21；kit Palette 的 Copy 值 |
| Peri 配色订阅 | `peri-theme/src/atoms.rs` | `PERI_COLORS_ATOM`:24；Arc 包装的 PeriColors |

本 crate 不实现 TUI 控件；实际主题加载与模式切换消费者见
[`peri-tui` 索引](peri-tui.md) 和 `peri-tui/src/kit/entry.rs`。

## 回归入口

| 场景 | 测试文件 | 关键用例 |
| --- | --- | --- |
| 共享引用不能误报循环 | `peri-theme/src/loader_test.rs` | `test_shared_reference_chain_is_not_a_cycle_in_any_iteration_order`；按实际 HashMap 遍历顺序构造确定性回归 |
| 非 ASCII hex 不得 panic | `peri-theme/src/loader_test.rs` | `test_unicode_hex_returns_error_without_panicking` |
| 默认值双来源一致 | `peri-theme/src/loader_test.rs` | `test_builtin_json_and_rust_definitions_match_every_token`；比较全部字段 |
| 旧 JSON 缺省字段兼容 | `peri-theme/src/loader_test.rs` | `test_old_theme_json_without_new_keys_still_loads` |
| 父子覆盖和来源优先级 | `peri-theme/src/loader/source_test.rs` | `inherits_before_resolving_references_and_preserves_child_identity`、`user_sources_override_builtins_for_root_and_parent_lookups` |
| 循环、缺失和最大继承深度 | `peri-theme/src/loader/source_test.rs` | `rejects_cyclic_missing_and_overdeep_parents` |
| 路径与 dotfiles 兼容性 | `peri-theme/src/loader/source_test.rs` | `rejects_paths_in_inherited_theme_names`、`root_lookup_retains_relative_and_absolute_paths`、`user_sources_retain_dotfiles_symlinks_for_root_and_parent` |
| 公共 API 与用户主题 | `peri-theme/tests/test_loader.rs` | `public_loader_uses_isolated_home`；子进程 HOME 指向测试目录，不访问真实用户配置 |
| 默认主题和 bridge | `peri-theme/tests/test_builtin.rs` | dark/light 字段与默认映射断言 |

## 跨模块契约

- TUI-THEME-001、TUI-RENDER-001：主题订阅和写入边界以 [`tui.md`](../standards/tui.md) 为准；本 crate 提供 atoms 和 bridge，消费者负责 hook 与非组件 guard 生命周期。
- 本次加载器边界不处理 ACP wire、会话身份或事件生命周期；跨层契约入口为 [`architecture-contracts.md`](../standards/architecture-contracts.md)。
- 验证命令：`cargo test -p peri-theme`、`cargo test -p peri-theme --doc`、`cargo clippy -p peri-theme --all-targets -- -D warnings`；消费者兼容验证用 `cargo check -p peri-tui`。
