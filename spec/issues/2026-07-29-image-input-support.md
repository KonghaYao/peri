# Agent 图片输入支持

**状态**：Fixed
**优先级**：高
**类型**：Bug
**创建日期**：2026-07-29

## 问题描述

当前 Agent 无法接收图片输入。虽然底层数据模型（`ContentBlock::Image`、`ImageSource`）和 Provider 适配器（Anthropic/OpenAI）均已就绪，但 TUI → ACP → Agent 这条管线的图片通道未打通：TUI 不会处理剪贴板图片，Agent 侧没有将图片路径转换为 `ContentBlock::Image` 的中间件。

## 设计

### 整体数据流

```
TUI (Ctrl+V)                           ACP                      Agent
─────────────                          ───                      ─────
检测剪贴板为图片                          │                        │
  ├─ 存到 ~/.peri/images/xxx.png        │                        │
  └─ 输入框插入 "@image <path>" 文本     │                        │
     └─── ACP prompt ──────────────────►│                        │
                                       └─── Agent 入队 ────────►│
                                                                │
                                              ImageMiddleware::before_agent
                                                                │
                                              ┌─ 扫描最新 user message 的 @image
                                              ├─ 路径校验、MIME 检测
                                              ├─ 大小校验
                                              ├─ 压缩管道（预留空管线）
                                              └─ 替换为 ContentBlock::Image
                                                                │
                                              Provider 适配器（已有，不改动）
```

**关键决策**：
- TUI 只传路径文本，不传 base64，ACP transport 保持轻量
- 中间件在 `before_agent` 阶段将路径转换为 `ContentBlock::Image`
- 压缩作为独立切面，trait 预留，MVP 不实现

### TUI 层：剪贴板粘贴

在现有粘贴处理中增加图片检测分支：

```
用户 Ctrl+V
  │
  ├─ 剪贴板内容为纯文本 → 现有逻辑
  │
  └─ 剪贴板内容为图片
       ├─ 生成文件名：{timestamp}_{hash前8位}.{ext}
       ├─ 保存到 ~/.peri/images/
       └─ 输入框插入 "@image <path>" 文本
```

- 拖拽场景不需要处理：终端拖拽文件会自动在输入框插入文件路径
- 只处理剪贴板图片这一个新场景

### Agent 层：ImageMiddleware

新增 `peri-middlewares/src/middleware/image.rs`，结构：

```rust
pub struct ImageMiddleware {
    image_dir: PathBuf,           // ~/.peri/images/
    max_size: usize,
    compressors: Vec<Box<dyn ImageCompressor>>,  // 预留切面
}
```

**生命周期钩子**：

| 钩子 | 职责 |
|---|---|
| `before_agent(state)` | 扫描最新 user message 中的 `@image <path>`，读取文件，替换为 `ContentBlock::Image` |
| `on_user_prompt(prompt)` | 做 @image 语法校验，提前报错（可选） |

**`before_agent` 核心流程**：

```
1. 取最新一条 user message
2. 正则匹配所有 @image <path> 模式
3. 对每个匹配：
   a. 展开 ~、解析相对路径
   b. 校验路径存在 → 不存在则替换为 "[Image not found: /path]"
   c. MIME 检测是否为图片 → 非图片替换为 "[Not an image: /path]"
   d. 检查文件大小 ≤ max_size → 超限替换为 "[Image too large: XMB > YMB limit]"
   e. 运行压缩链（当前为空）
   f. 读取文件 → base64 编码 → ContentBlock::Image
4. 重构消息：删除所有 @image 文本片段，追加 Image 块
5. 写回 state
```

**@image 语法**：`@image /path/to/photo.png`，空格分隔，路径到行尾。支持 `~` 和相对路径。

### 压缩切面（预留）

```rust
pub trait ImageCompressor: Send + Sync {
    fn name(&self) -> &str;
    fn compress(&self, data: &[u8], media_type: &str) -> Result<Vec<u8>>;
}
```

MVP 阶段不实现任何压缩器，管道留空。后续可接入尺寸缩放、JPEG 质量压缩、PNG 量化等。

### 边界情况

| 场景 | 处理 |
|---|---|
| 路径不存在 | 替换为 `[Image not found: /path]` 文本，不阻断流程 |
| 非图片文件 | 替换为 `[Not an image: /path]` |
| 文件过大 | 超过 max_size → 替换为 `[Image too large: XMB > YMB limit]` |
| 多张图片 | 逐张处理，追加多个 Image 块 |
| @image 在历史消息中 | 只处理最新一条 user message |
| Compact | 已有逻辑：`ContentBlock::Image` → `[Image: {source}]` 占位 |
| 多次粘贴同一图片 | 每次生成独立文件（timestamp 不同），无冲突 |
| 不注入 system prompt | LLM 天然能理解图片内容，无需额外声明 |

## 实施计划

### 文件清单

| 文件 | 操作 | 内容 |
|---|---|---|
| `peri-tui/src/kit/input_paste.rs` | 新增/修改 | 剪贴板图片检测、存文件、插 @image 文本 |
| `peri-middlewares/src/middleware/image.rs` | 新增 | ImageMiddleware 主体 |
| `peri-middlewares/src/middleware/image/compressor.rs` | 新增 | ImageCompressor trait + 空管线 |
| `peri-middlewares/src/middleware/mod.rs` | 修改 | 注册 `pub mod image` |
| `peri-acp/src/session/builder.rs` | 修改 | 装配 ImageMiddleware 到中间件链 |
| `peri-middlewares/Cargo.toml` | 修改 | 按需添加依赖（`image`、`regex`） |

### 步骤

1. **TUI 剪贴板图片处理** — 检测剪贴板图片 → 保存到 `~/.peri/images/` → 插入 `@image` 文本
2. **ImageMiddleware** — `before_agent` 钩子扫描 @image → 读文件 → 替换为 ContentBlock::Image
3. **压缩切面** — `ImageCompressor` trait + 空管线，为后续实现留接口
4. **注册装配** — 中间件链注册 + ACP builder 装配
5. **集成验证** — 端到端测试：粘贴图片 → 发送 → Agent 看到图片

## 验证标准

- [ ] TUI 粘贴剪贴板图片后在输入框出现 `@image <path>` 文本
- [ ] 图片文件保存到 `~/.peri/images/` 且内容正确
- [ ] Agent 接收消息后 `ContentBlock::Image` 出现在 user message 中
- [ ] Anthropic provider 正确将图片序列化到 API 请求
- [ ] 不存在的路径显示 `[Image not found: ...]`
- [ ] 非图片文件显示 `[Not an image: ...]`
- [ ] 超限文件显示 `[Image too large: ...]`
- [ ] 现有工具注册测试和对 Compact 测试不受影响

## 症状详情

### 2026-07-30：图片识别完全无效

用户 @image 粘贴 PNG 后发送消息：

```text
@image /Users/konghayao/.peri/images/20260730-124853_e7605bf9.png 这个是什么
```

**实际行为**：Agent 调用 `Read` 工具试图以文本方式读取 PNG 文件，返回少量二进制行，随后回复"我目前只能确认这是一个 PNG 图片文件，无法直接看到其中的画面内容"。

**症状特征**：
- `@image <path>` 未被 ImageMiddleware 转换为 `ContentBlock::Image`
- 用户消息原文（含 @image 前缀）直接发送到模型
- 模型将路径视为本地文件，用 Read 工具打开
- 剪贴板粘贴和手动 @image 路径都无效

**根因推测**：ImageMiddleware 未注册/未生效，或 before_agent 钩子未执行 @image → ContentBlock::Image 的转换。

## 状态变更记录

| 日期 | 旧状态 | 新状态 | 操作人 | 备注 |
|------|--------|--------|--------|------|
| 2026-07-30 | Open | Reopen | agent | 用户反馈图片识别完全无效，@image 未被转换 |
| 2026-07-30 | Reopen | Fixed | agent | 修复 messages_mut() 不同步到 transcript 的桥接问题 |

## 修复记录

### 修复 #1（2026-07-30）

- **操作人**：agent
- **用户原意**：@image 粘贴图片后 Agent 应能识别图片内容
- **修复内容**：
  1. `peri-agent/src/session/transcript.rs`：新增 `replace_by_id()` 方法，支持按 MessageId 替换条目
  2. `peri-agent/src/agent/agent_context.rs`：新增 `messages_cache()`、`messages_modified()`、`reconcile_to_transcript()` 方法；`messages_mut()` 设置脏标记
  3. `peri-agent/src/agent/stages/middleware_runner.rs`：`run_before_agent()` 执行后调用 `reconcile_to_transcript()` 将缓存变更同步到 transcript
- **涉及 commit**：待提交
- **验证状态**：待验证
