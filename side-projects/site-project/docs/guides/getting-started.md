# 使用指南

## 目录结构

```
site-project/
├── server.ts          # Bun 后端服务
├── package.json
├── tsconfig.json
├── public/            # 前端静态资源
│   ├── index.html
│   ├── app.js         # 主入口
│   ├── tree.js        # 文件树组件
│   ├── preview.js     # 预览组件
│   └── style.css
└── docs/              # 要展示的文档内容
    └── ...
```

## 添加文档

直接将文件放入 `docs/` 目录即可，服务器会自动扫描。

### 支持的格式

- `.md` — Markdown（含 Mermaid、代码高亮）
- `.js` / `.ts` / `.py` / `.rs` 等 — 代码高亮预览
- `.png` / `.jpg` / `.gif` / `.svg` / `.webp` — 图片预览
- `.html` — 源代码高亮预览
- `.json` / `.xml` / `.yaml` — 结构化文本预览

## 自定义端口

```bash
PORT=8080 bun run dev
```

修改 `server.ts` 中的 `PORT` 常量即可。
