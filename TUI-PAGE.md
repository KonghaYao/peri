# TUI 页面能力设计图

> **此文档已迁移至 `spec/global/domains/tui/` 子目录。**
>
> 请参见各部分：
> - [总索引](spec/global/domains/tui/tui-index.md) — 领域综述、技术方案、面板导航、快捷键规范
> - [渲染系统](spec/global/domains/tui/tui-rendering.md) — AppShell、MessageArea、StatusBar、BgTaskArea
> - [ACP 事件系统](spec/global/domains/tui/tui-events.md) — 事件分派管线、acp_bridge、ACP Data Flow
> - [输入系统](spec/global/domains/tui/tui-input.md) — InputArea、@mention、slash 命令、软换行
> - [面板系统](spec/global/domains/tui/tui-panels.md) — PanelOverlay、16 Panel、导航互斥
> - [弹窗系统](spec/global/domains/tui/tui-popups.md) — PopupOverlay、HITL、AskUser、OAuth

## Model Panel：Profile 配置

### 数据关系

`Profile` 是独立的模型档位。每个 Profile 指向一个 `provider + model`，并独立保存以下配置；切换 Profile 时整组配置生效，不与其他 Profile 共享：

- `provider + model`
- `effort`
- `max tokens`
- `1m enable`

内置 Profile 固定按以下顺序显示：

```text
fable
opus
sonnet
haiku
```

所有 Profile 的字段都可以在 Model Panel 中独立修改。

### 面板布局

Model Panel 使用左右分栏：左侧选择 Profile，右侧编辑当前 Profile 的配置。

```text
┌─ Model ─────────────────────────────────────────────────────────────────────┐
│                                                                              │
│  Profiles                         fable · openai                            │
│  ─────────────────────────        ─────────────────────────────────────────  │
│ ❯ fable · openai                  Provider                         openai    │
│   gpt-5.6-luna high               Model                    gpt-5.6-luna    │
│   high · 1m                       Effort                              high    │
│                                    Max tokens                          1m    │
│   opus · anthropic                1m enable                            on    │
│   claude-opus-4-6                                                            │
│   high · 200k                      Tab::左右  ←/→::改值  Esc::退出/关闭      │
│                                                                              │
│   sonnet · openai                                                              │
│   gpt-5.6-luna high                                                           │
│   high · 1m                                                                  │
│                                                                              │
│   haiku · anthropic                                                           │
│   claude-haiku-4-5                                                            │
│   medium · 200k                                                               │
└──────────────────────────────────────────────────────────────────────────────┘
```

左侧 Profile 摘要只显示三行：

1. `profile · provider`
2. 模型名称
3. `effort · 200k` 或 `effort · 1m`

不显示额外说明，也不使用 `key: value` 格式。active Profile 使用现有高亮样式。

### 右侧编辑

右侧字段使用单行 K/V 布局：字段名左对齐，值右对齐；不使用方括号或其他包围符号。使用左右方向键切换当前字段的值，切换即写入内存并持久化（无 Enter/Save 步骤）。`Tab` 在左右栏焦点间切换（`→` 也可从左侧进入右侧）；右侧按 `Esc` 退出到左侧焦点，左侧按 `Esc` 关闭面板。

`Provider` 与 `Model` 联动：切换 Provider 后，Profile 的 Model 联动为同档位——优先取目标 Provider 的同档位模型（fable 空回退 opus），该档位未配置时取目标 Provider 的默认 Model。不做模型能力兼容性检查。

模型名称 `gpt-5.6-luna high` 中的 `high` 使用 model accent color；摘要中的 `effort` 值使用独立的 effort color，二者不可复用同一颜色语义。
