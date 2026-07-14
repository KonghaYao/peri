# Theme 系统重新设计

**日期**: 2026-07-10
**状态**: 设计完成，待实施

## 背景

当前 Perihelion 存在严重的"双主题系统"分裂问题：

- `peri-tui::kit::theme::ThemeDefinition`（多层 struct，编译期静态）— kit 组件消费
- `peri-widgets::theme::Theme`（扁平 trait）— widgets 消费
- 两套系统互不兼容，切换其中一套不影响另一套
- DarkTheme 在 widgets 中硬编码，markdown palette 从独立 hex 构造
- feature/theme-system worktree 有 JSON 驱动的方向但未集成

## 目标

1. **统一**：消除双系统分裂，所有组件走同一套 token 体系
2. **动态切换**：运行时切换 dark/light/自定义主题，组件响应式更新
3. **JSON 可定制**：用户通过 `~/.peri/themes/*.json` 创建自定义主题
4. **ratatui-kit 全量对接**：完全走 ratatui-kit 的 Palette + ComponentTheme 体系

## 设计决策

| 决策 | 选择 |
|------|------|
| 核心目标 | 统一系统 + 动态切换 |
| 数据源 | 内置 Rust 定义 + 用户 JSON 动态加载 |
| crate 关系 | 合并为独立 `peri-theme` crate |
| Token 粒度 | 结构化 `ThemeDefinition`（分组层级） |
| 层级结构 | 完整三层：Palette → Semantic → Component |
| ratatui-kit 对接 | 全部重构（包括 peri-widgets） |
| Peri 特有颜色 | `Palette` + 全局 `Atom<PeriColors>` |
| JSON 覆盖范围 | 任意层任意键 |
| JSON 格式 | 扁平键路径（`palette.brand.primary`） |
| 架构方案 | Derived Atoms（THEME_ATOM + PALETTE_ATOM + PERI_COLORS_ATOM） |

## 架构

### Crate 结构

```
peri-theme/          (新 crate)
├── src/
│   ├── lib.rs           (re-export)
│   ├── palette.rs       (Palette 颜料盘)
│   ├── semantic.rs      (SemanticTokens)
│   ├── component.rs     (ComponentTokens)
│   ├── theme.rs         (ThemeDefinition)
│   ├── builtin.rs       (内置 dark/light)
│   ├── loader.rs        (JSON 加载 + $ref)
│   ├── atoms.rs         (三 Atom)
│   ├── bridge.rs        (→ Palette + PeriColors)
│   └── peri_colors.rs   (特有颜色)
├── themes/
│   ├── dark.json
│   └── light.json
└── tests/
```

**删除**:
- `peri-tui/src/kit/theme/` 整目录（~450 行）
- `peri-widgets/src/theme/` 整目录（~120 行）

**依赖关系**: `peri-tui` 和 `peri-widgets` 都依赖 `peri-theme`

### 数据类型

```rust
pub struct ThemeDefinition {
    pub name:      String,
    pub mode:      ThemeMode,
    pub palette:   Palette,
    pub semantic:  SemanticTokens,
    pub component: ComponentTokens,
}
```

三层结构保留当前 kit/theme 的设计，无结构性变更。

### JSON 格式

扁平键路径，支持 `$ref` 别名引用和 `extends` 继承：

```json
{
  "name": "my-theme",
  "extends": "peri-dark",
  "palette.brand.primary": "#D77757",
  "semantic.text.primary": "$palette.base.fg",
  "semantic.border.active": "$palette.accent.primary",
  "component.message.user_bg": "#373737"
}
```

解析顺序：palette → semantic（可引用 palette） → component（可引用 palette + semantic）。

### Atom 系统

```
THEME_ATOM:      Atom<Arc<ThemeDefinition>>   — 数据源头
PALETTE_ATOM:    Atom<Palette>               — ratatui-kit 组件消费
PERI_COLORS_ATOM: Atom<Arc<PeriColors>>       — Peri 特有颜色
```

切换流程：
1. `ThemeLoader::load(name)` → `Arc<ThemeDefinition>`
2. `THEME_ATOM.set(theme)`
3. `PALETTE_ATOM.set(theme.to_palette())` + `PERI_COLORS_ATOM.set(theme.to_peri_colors())`
4. 所有订阅 atom 的 ratatui-kit 组件自动重渲染

### Bridge

```rust
impl ThemeDefinition {
    pub fn to_palette(&self) -> Palette { ... }
    pub fn to_peri_colors(&self) -> PeriColors { ... }
}
```

## 迁移计划（5 步）

### Step 1: 创建 peri-theme crate
- 搬入类型定义、builtin 主题、JSON loader、bridge、三 Atom
- 验证：`cargo build -p peri-theme` + 单元测试

### Step 2: peri-widgets 切换
- 删除旧 theme 目录，组件改为接收 ThemeDefinition/颜色参数
- 验证：`cargo build -p peri-widgets`

### Step 3: peri-tui kit 组件切换
- 删除旧 theme 目录，组件改为从 atom 取色，AppShell 改 PaletteProvider
- 验证：`cargo build -p peri-tui`

### Step 4: /theme 命令 + 配置对接
- /theme 面板、AppConfig.theme 字段、启动加载
- 验证：手动切换 dark ↔ light

### Step 5: 清理 + 全量测试
- 删除 worktree 过时代码，`cargo test --workspace && cargo clippy --workspace`

**净变化**: ~−250 行

## 风险

| 风险 | 缓解 |
|------|------|
| 删 theme 目录导致大量编译错误 | 先建 peri-theme 再逐步替换 |
| 颜色切换时某些组件未响应 | 每步 build 后手动测试 |
| JSON 加载失败不回落 | loader 保证 fallback 到 builtin::dark_theme() |
