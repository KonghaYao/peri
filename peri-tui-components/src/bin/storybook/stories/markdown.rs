//! Markdown 样例；围栏仍通过同一条文档渲染链路。

use super::{Variant, document_state};

// 行尾两个空格是 Markdown 硬换行的语义字节，用转义拼接避免源文件里的行尾空白。
const MARKDOWN_RICH_TEXT: &str = concat!(
    r#"# Peri 富文本渲染

这一整段用来观察自动换行与标点节奏：中文按显示宽度折行，英文单词保持完整，行内样式不会破坏断词。

**粗体**、*斜体*、***粗斜体***、~~删除线~~ 与 `inline code` 可以自由组合；行内链接写作 [Peri 文档](https://example.com/docs)，自动链接写作 <https://example.com/auto>。

硬换行由行尾两个空格触发，下一行仍属于同一段落："#,
    "  \n",
    r#"这一行紧接在上一行之后，但保留了视觉换行。

Soft break
folds into a single space, which keeps English paragraphs compact.

引用式链接同样生效：[Peri 组件文档][components]，定义行不会进入正文。

转义保留字面量：\*不是斜体\*、\_不是下划线\_；实体渲染为字符：&amp; &lt; &gt;。
行内 HTML 标签被剥离但保留文本：<kbd>Ctrl</kbd>+<kbd>C</kbd>。

## 二级标题

三级及以下标题共用加粗主文本样式，依靠空行节奏区分层级。

### 三级标题

#### 四级标题

##### 五级标题

###### 六级标题

Setext 一级标题
===============

Setext 二级标题
---------------

## 引用

> 引用块的第一段：渲染器把引用内容展平为普通段落。
>
> > 嵌套引用第二层，深度只影响解析，不额外加竖线。
>
> 引用内的列表：
>
> - 引用列表项 A
> - 引用列表项 B

## 列表

- 无序一级
  - 嵌套二级
    - 嵌套三级
  - 回到二级
- 第二项，含 **粗体**、`code` 与 [链接](https://example.com/list)

3. 有序列表从 3 开始编号
4. 第二项
5. 第三项

## 代码

```rust
fn render_markdown(source: &str) -> Vec<Line<'static>> {
    let segments = parse_markdown(source, 80, palette, base_fg);
    segments.into_iter().flat_map(flatten).collect()
}
```

```json
{
  "name": "peri",
  "features": ["markdown", "openui"]
}
```

```diff
- old line
+ new line
  context line
```

```bash
cargo test -p peri-tui-components --offline
cargo clippy --all-targets -- -D warnings
```

```unknown-language
unknown info string falls back to plain text
second line keeps the block chrome
```

无语言围栏同样降级为纯文本：

```
plain fence without an info string
another plain line
```

## 表格

| 左对齐 | 居中 | 右对齐 |
| :--- | :---: | ---: |
| Markdown | 12 | 3 |
| 表格 | 宽字符 | 1,024 |
| 空单元格 |  | 7 |

列宽由最长不可断词决定；超出可用宽度时按显示宽度折行。

| 组件 | 覆盖 |
| --- | --- |
| Markdown | 标题、段落、列表、代码、表格、图片 |
| OpenUI | Stack、Card、Text、Heading、Progress |

## 分隔线

---

## 图片

![本地架构图](docs/architecture.png "架构")

本地图片按相对路径展示，alt 文本进入降级标签。

![远程示意图](https://example.com/diagram.png)

[components]: https://example.com/components

## OpenUI 围栏

```gen-ui
root = Stack([
  Card([
    Heading("Peri 富文本", 2),
    Text("Markdown 段落、表格与 OpenUI 围栏共用同一条渲染管线", "muted"),
    Progress("覆盖度", 92, 100)
  ], "组件预览")
])
```
"#
);

const MARKDOWN_HEADINGS: &str = r#"# 一级标题 H1

H1 之后的正文段落，用于观察标题与正文之间的空行节奏。

## 二级标题 H2

### 三级标题 H3

#### 四级标题 H4

##### 五级标题 H5

###### 六级标题 H6

Setext 一级
===========

Setext 二级
-----------

## 标题中的行内样式：**加粗**、*斜体* 与 `code`

标题不渲染 `#` 前缀，行内样式与正文共用同一套转换。
"#;

const MARKDOWN_LISTS: &str = r#"## 列表

- 无序一级
  - 嵌套二级
    - 嵌套三级
    - 同级第二项
  - 回到二级
- 第二项，含 **粗体**、`code` 与 [链接](https://example.com/list)
- 第三项
  多行段落跟随列表项，缩进由解析器归一。

3. 有序列表从 3 开始编号
4. 第二项
5. 第三项

长列表用于验证滚轮：

- 长列表项 01：每次滚轮移动三行
- 长列表项 02：到达末尾后不再继续
- 长列表项 03：回到顶部后不再继续
- 长列表项 04：点击正文不改变滚动位置
- 长列表项 05：右侧滚动条随位置移动
- 长列表项 06：内容不足时滚动条隐藏
"#;

const MARKDOWN_CODE: &str = r#"## 代码块

```rust
fn render_markdown(source: &str) -> Vec<Line<'static>> {
    let segments = parse_markdown(source, 80, palette, base_fg);
    segments.into_iter().flat_map(flatten).collect()
}
```

```json
{
  "name": "peri",
  "features": ["markdown", "openui"]
}
```

```diff
- old line
+ new line
  context line
```

```bash
cargo test -p peri-tui-components --offline
cargo clippy --all-targets -- -D warnings
```

```unknown-language
unknown info string falls back to plain text
second line keeps the block chrome
```

无语言围栏同样降级为纯文本：

```
plain fence without an info string
another plain line
```

超长行按显示宽度折行，续行不重复左侧竖线：

```text
0123456789 0123456789 0123456789 0123456789 0123456789 0123456789 0123456789
```
"#;

const MARKDOWN_TABLE: &str = r#"## 表格

| 左对齐 | 居中 | 右对齐 |
| :--- | :---: | ---: |
| Markdown | 12 | 3 |
| 表格 | 宽字符 | 1,024 |
| 空单元格 |  | 7 |

列宽由最长不可断词决定；超出可用宽度时按显示宽度折行。

| 组件 | 覆盖 |
| --- | --- |
| Markdown | 标题、段落、列表、代码、表格、图片 |
| OpenUI | Stack、Card、Text、Heading、Progress |
"#;

const MARKDOWN_IMAGE: &str = r#"## 图片降级

![本地架构图](docs/architecture.png "架构")

本地图片按相对路径展示，alt 文本进入降级标签。

![远程示意图](https://example.com/diagram.png)

远程图片按 scheme 分类，不发起任何网络请求。

![](images/screenshot.png)

无 alt 文本的图片同样给出占位标签。
"#;

const MARKDOWN_GENUI_FENCE: &str = r#"## OpenUI fence

下面是一个 gen-ui 围栏中的卡片：

```gen-ui
root = Stack([
  Card([
    Heading("构建状态", 2),
    Text("Markdown → GenUI → 终端", "muted"),
    Progress("完成度", 80, 100)
  ], "概览")
])
```
"#;

pub(super) const MARKDOWN_VARIANTS: &[Variant] = &[
    Variant {
        key: "rich-text",
        note: "Every supported construct in one scrollable document.",
        initial: || document_state(MARKDOWN_RICH_TEXT, 0),
    },
    Variant {
        key: "headings",
        note: "H1-H6 and setext headings, including inline styles.",
        initial: || document_state(MARKDOWN_HEADINGS, 0),
    },
    Variant {
        key: "lists",
        note: "Nested ordered/unordered lists, inline styles and long scrolling.",
        initial: || document_state(MARKDOWN_LISTS, 0),
    },
    Variant {
        key: "code",
        note: "Rust/JSON/diff/bash highlighting, plain fallbacks, long-line wrap.",
        initial: || document_state(MARKDOWN_CODE, 0),
    },
    Variant {
        key: "table",
        note: "Unicode grid: left/center/right, CJK and empty cells.",
        initial: || document_state(MARKDOWN_TABLE, 0),
    },
    Variant {
        key: "image",
        note: "Local, remote and empty-alt images degrade to text.",
        initial: || document_state(MARKDOWN_IMAGE, 0),
    },
    Variant {
        key: "genui-fence",
        note: "A fenced gen-ui block inside Markdown renders as OpenUI.",
        initial: || document_state(MARKDOWN_GENUI_FENCE, 0),
    },
    Variant {
        key: "streaming",
        note: "逐块到达的 Markdown 渐进渲染；dump 固定在中段。",
        initial: super::streaming::markdown_streaming_initial,
    },
];
