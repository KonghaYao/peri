# Site Project — 文档站点预览系统

基于 **Bun + TypeScript** 的轻量级文档预览站点。

## 功能特性

- 📁 **侧边栏文件树** —— 递归遍历 `docs/` 目录，支持折叠/展开
- 📝 **Markdown 全功能预览** —— 表格、代码高亮、引用块、列表
- 🎨 **Mermaid 图表** —— 流程图、时序图、甘特图
- 💻 **代码高亮** —— 支持 30+ 语言
- 🖼️ **图片预览** —— PNG / JPG / GIF / SVG / WebP
- 🎯 **拖拽调整** —— 侧边栏宽度可拖拽

## 快速开始

```bash
cd site-project
bun install
bun run dev
```

访问 `http://localhost:3000`

## Mermaid 示例

### 流程图

```mermaid
graph TD
    A[用户请求] --> B{路由判断}
    B -->|静态文件| C[返回 public/ 文件]
    B -->|/api/tree| D[递归遍历 docs/]
    B -->|/api/file| E[读取文件内容]
    D --> F[返回 JSON 树]
    E --> G[返回文本/二进制]
```

### 时序图

```mermaid
sequenceDiagram
    participant 浏览器
    participant 服务器
    participant 文件系统

    浏览器->>服务器: GET /api/tree
    服务器->>文件系统: readdir docs/
    文件系统-->>服务器: 目录结构
    服务器-->>浏览器: JSON Tree

    浏览器->>服务器: GET /api/file?path=README.md
    服务器->>文件系统: read file
    文件系统-->>服务器: 文件内容
    服务器-->>浏览器: { content, language, size }
```

### 甘特图

```mermaid
gantt
    title 开发计划
    dateFormat  YYYY-MM-DD
    section 后端
    API 开发           :a1, 2024-01-01, 3d
    文件服务           :a2, after a1, 2d
    section 前端
    文件树组件         :b1, 2024-01-01, 2d
    预览组件           :b2, after b1, 3d
    样式优化           :b3, after b2, 2d
```

## 代码示例

### TypeScript

```typescript
interface FileNode {
  name: string;
  path: string;
  type: "file" | "directory";
  children?: FileNode[];
}

async function buildTree(dirPath: string): Promise<FileNode[]> {
  const entries = await readdir(dirPath, { withFileTypes: true });
  // ...
}
```

### Python

```python
def fibonacci(n: int) -> list[int]:
    """生成斐波那契数列"""
    a, b = 0, 1
    result = []
    for _ in range(n):
        result.append(a)
        a, b = b, a + b
    return result

print(fibonacci(10))
# [0, 1, 1, 2, 3, 5, 8, 13, 21, 34]
```

### Rust

```rust
use std::collections::HashMap;

fn main() {
    let mut map = HashMap::new();
    map.insert("key", "value");

    for (k, v) in &map {
        println!("{}: {}", k, v);
    }
}
```

## 表格示例

| 特性 | 支持 | 备注 |
|------|------|------|
| Markdown | ✅ | marked.js 渲染 |
| Mermaid | ✅ | 流程图/时序图/甘特图 |
| 代码高亮 | ✅ | highlight.js 30+ 语言 |
| 图片预览 | ✅ | PNG/JPG/GIF/SVG/WebP |
| HTML 预览 | 🔜 | 计划中 |
| PDF 预览 | ❌ | 不支持 |

## 引用块

> "Any application that can be written in JavaScript, will eventually be written in JavaScript."
> — Atwood's Law

> [!NOTE]
> 这是一个提示块，用于强调重要信息。

> [!WARNING]
> 这是警告块，注意安全风险。

## 任务列表

- [x] 后端 API 开发
- [x] 文件树组件
- [x] Markdown 预览
- [x] Mermaid 支持
- [ ] HTML 实时预览
- [ ] 搜索功能
