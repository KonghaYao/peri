# Agent 图片输入支持 — 实施计划

> **给实施者：** 按任务顺序执行；每步包含精确代码和命令。使用 superpowers:subagent-driven-development 或 superpowers:executing-plans 按任务逐步实现。
>
> **来源：** `spec/issues/2026-07-29-image-input-support.md`
>
> **目标：** 打通 TUI 剪贴板图片 → @image 文本 → ImageMiddleware 转换 → ContentBlock::Image → Provider 适配器的完整管线。压缩切面预留 trait + 空管线，MVP 不实现具体压缩器。

**架构：** TUI（arboard 检测图片 → 存 ~/.peri/images/ → 插 @image 文本）→ ACP（纯文本传输）→ ImageMiddleware::before_agent（解析 @image → 读文件/base64 编码 → 替换为 ContentBlock::Image）

**技术栈：** arboard 3.6.1 (已有)、base64 0.22 (已有)、png 0.18 (已有)、regex 1 (已有)、image crate (新增依赖，用于 MIME 检测)

---

## 文件清单

| 文件 | 操作 | 职责 |
|---|---|---|
| `peri-tui/src/kit/input_area.rs` | 修改 | Ctrl+V 增加图片检测分支 |
| `peri-middlewares/src/middleware/image.rs` | 新建 | ImageMiddleware 主体 |
| `peri-middlewares/src/middleware/image/compressor.rs` | 新建 | ImageCompressor trait + 空管线 |
| `peri-middlewares/src/middleware/mod.rs` | 修改 | 注册 image 模块 |
| `peri-acp/src/agent/builder.rs` | 修改 | 装配 ImageMiddleware 到链 |
| `peri-middlewares/Cargo.toml` | 修改 | 添加 `image` 依赖 |

---

### Task 1: TUI 剪贴板图片检测与保存

**文件：**
- 修改：`peri-tui/src/kit/input_area.rs`

**背景：** 当前 `input_area.rs:389-415` 的 Ctrl+V 分支只读取文本剪贴板（`cb.get_text()`）。需要增加图片检测：先尝试 `get_image()`，如果有图片则保存文件并插入 `@image <path>` 文本；否则回退到现有文本粘贴逻辑。

> **CRITICAL: 读取完整的 `peri-tui/src/kit/input_area.rs:385-430` 后再编辑，确认当前剪贴板处理块的精确边界。**

- [ ] **Step 1: 研读现有粘贴代码**

```bash
# 确认当前 Ctrl+V 代码块的精确行号范围和结构
grep -n "Ctrl+V\|get_text\|arboard::Clipboard" peri-tui/src/kit/input_area.rs
```

- [ ] **Step 2: 在 Ctrl+V 分支开头增加图片检测**

在 `let Ok(mut cb) = arboard::Clipboard::new()` 之后、`cb.get_text()` 之前，插入图片检测逻辑：

```rust
// 在独立线程读 arboard（阻塞系统 I/O 不卡 UI），通过 state clone 回写 editor。
// 粘贴不应触发 slash/mention 弹窗（与 Event::Paste 分支一致）。
KeyCode::Char('v')
    if is_ctrl && !is_alt && !is_shift && !mention_active && !slash_active =>
{
    exit_history_mode_if_active();
    let state_clone = state;
    std::thread::spawn(move || {
        let Ok(mut cb) = arboard::Clipboard::new() else {
            return;
        };
        // ── 图片粘贴分支 ──
        if let Ok(image_data) = cb.get_image() {
            // 将图片数据转换为 PNG 字节（arboard 返回的是 ImageData { width, height, bytes }）
            use png::{ColorType, Encoder};
            let mut png_bytes = Vec::new();
            {
                let mut encoder = Encoder::new(
                    &mut png_bytes,
                    image_data.width as u32,
                    image_data.height as u32,
                );
                encoder.set_color(ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                // 编码器实现
            }
            // 如果 PNG 编码失败，静默回退
            // ...保存到 ~/.peri/images/{timestamp}_{hash}.png 的逻辑
        }
        // ── 文本粘贴分支（原有逻辑）──
        let Ok(text) = cb.get_text() else {
            return;
        };
        // ... 现有文本粘贴代码 ...
    });
    EventResult::Consumed
}
```

- [ ] **Step 3: 实现完整的图片粘贴逻辑**

把 Step 2 的占位符替换为完整实现：

```rust
KeyCode::Char('v')
    if is_ctrl && !is_alt && !is_shift && !mention_active && !slash_active =>
{
    exit_history_mode_if_active();
    let state_clone = state;
    std::thread::spawn(move || {
        // ── 图片粘贴分支 ──
        // 先尝试获取剪贴板图片
        if let Ok(image_data) = arboard::Clipboard::new()
            .ok()
            .and_then(|mut cb| cb.get_image().ok())
        {
            let img_bytes = image_data.bytes.to_vec();
            if !img_bytes.is_empty() {
                use std::hash::{DefaultHasher, Hash, Hasher};
                let mut hasher = DefaultHasher::new();
                img_bytes.hash(&mut hasher);
                let hash = format!("{:016x}", hasher.finish());
                let timestamp = chrono::Utc::now().format("%Y%m%d-%H%M%S");

                let img_dir = dirs_next::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".peri")
                    .join("images");
                if let Err(_e) = std::fs::create_dir_all(&img_dir) {
                    return;
                }

                let file_name = format!("{}_{}.png", timestamp, &hash[..8]);
                let file_path = img_dir.join(&file_name);

                // 将 RGBA 数据编码为 PNG
                match png_encode(&img_bytes, image_data.width, image_data.height, &file_path) {
                    Ok(()) => {
                        let at_text = format!("@image {}", file_path.display());
                        let (update_tx, _) = tokio::sync::oneshot::channel();

                        // Get editor state and insert text
                        let state_guard = state_clone.state();
                        let state_val = state_guard.read();
                        let editor_state = state_val.editor.state();
                        let editor_state_guard = editor_state.read();
                        let mut editor = editor_state_guard.editor.clone();
                        drop(editor_state_guard);
                        drop(state_val);
                        drop(state_guard);

                        editor.lock().insert_str(&at_text);

                        let mut state_guard = state_clone.state().write();
                        let state_val = state_guard.editor.state();
                        let mut editor_state = state_val.write();
                        editor_state.editor = editor;
                        let _ = state_clone.state().wake();
                    }
                    Err(_) => {} // PNG 编码失败，静默回退到文本粘贴
                }
                return;
            }
        }

        // ── 文本粘贴分支（原有逻辑）──
        let Ok(mut cb) = arboard::Clipboard::new() else {
            return;
        };
        let Ok(text) = cb.get_text() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        // ... 保持现有文本粘贴代码不变 ...
        const MAX: usize = 10_000;
        let total = text.chars().count();
        if total > MAX {
            *crate::kit::atoms::NOTIFICATION.state().write() =
                Some(crate::kit::atoms::Notification {
                    message: i18n::tr_args(
                        "paste-truncated",
                        &[("max".into(), FluentValue::from(MAX as i64))],
                    ),
                /* ... */
                });
            let text = text.chars().take(MAX).collect::<String>();
            // ... insert text into editor ...
        } else {
            // ... insert text into editor ...
        }
    });
    EventResult::Consumed
}
```

**注意：** 上述代码中的文本粘贴分支需保留原有完整代码。修改时需要先 Read 整个函数，确认代码块边界和缩进，然后再编辑。

- [ ] **Step 4: 添加 png_encode 辅助函数**

在同一文件的模块级（文件底部或顶部）添加：

```rust
/// 将 RGBA 字节数组编码为 PNG 文件
fn png_encode(
    rgba_bytes: &[u8],
    width: usize,
    height: usize,
    output_path: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = std::fs::File::create(output_path)?;
    let ref mut w = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(w, width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba_bytes)?;
    writer.finish()?;
    Ok(())
}
```

- [ ] **Step 5: 检查 arboard ImageData 读取逻辑**

arboard 3.6.1 在 macOS 上的 `get_image()` 返回 `ImageData<'static>`，其 `bytes` 字段是 `Cow<'static, [u8]>`，需要 `to_vec()` 才能 owned。确认上面的 `image_data.bytes.to_vec()` 能编译。

```rust
// arboard ImageData 类型：
// pub struct ImageData<'a> {
//     pub width: usize,
//     pub height: usize,
//     pub bytes: Cow<'a, [u8]>,
// }
```

- [ ] **Step 6: 编译验证**

```bash
cargo check -p peri-tui 2>&1
# 预期：编译通过，无新增 warning
```

- [ ] **Step 7: 提交**

```bash
git add peri-tui/src/kit/input_area.rs
git commit -m "feat(tui): Ctrl+V 增加剪贴板图片粘贴支持

粘贴时先检测剪贴板是否为图片，若是则存为 PNG 到
~/.peri/images/ 并在输入框插入 @image <path> 文本。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 2: ImageMiddleware 主体实现

**文件：**
- 新建：`peri-middlewares/src/middleware/image.rs`
- 新建：`peri-middlewares/src/middleware/image/compressor.rs`
- 修改：`peri-middlewares/src/middleware/mod.rs`
- 修改：`peri-middlewares/Cargo.toml`

- [ ] **Step 1: 在 Cargo.toml 中添加 `image` 依赖**

在 `peri-middlewares/Cargo.toml` 的 `[dependencies]` 段中添加：

```toml
image = { version = "0.25", default-features = false, features = ["png", "jpeg", "gif", "webp"] }
```

- [ ] **Step 2: 创建压缩切面模块**

创建 `peri-middlewares/src/middleware/image/compressor.rs`：

```rust
//! 图片压缩切面 —— 为后续实现预留接口，MVP 空管线。

use std::error::Error;

/// 图片压缩器 trait
///
/// 每个压缩器实现一种压缩策略（尺寸缩放、JPEG 质量、PNG 量化等）。
/// 管线上各压缩器按注册顺序依次执行。
pub trait ImageCompressor: Send + Sync {
    /// 压缩器名称（用于日志/调试）
    fn name(&self) -> &str;

    /// 对图片字节进行压缩
    ///
    /// # 参数
    /// - `data`: 原始图片字节
    /// - `media_type`: MIME 类型（如 "image/png"）
    ///
    /// # 返回
    /// - `Ok(compressed_bytes)`: 压缩后的字节
    /// - `Err(_)`: 压缩失败（此时应降级使用原始数据）
    fn compress(&self, data: &[u8], media_type: &str) -> Result<Vec<u8>, Box<dyn Error + Send + Sync>>;
}

/// 压缩管线 —— 依次执行多个压缩器
pub struct CompressorPipeline {
    compressors: Vec<Box<dyn ImageCompressor>>,
}

impl CompressorPipeline {
    /// 创建空管线（MVP 默认无压缩器）
    pub fn new() -> Self {
        Self {
            compressors: Vec::new(),
        }
    }

    /// 添加压缩器
    pub fn add(&mut self, compressor: Box<dyn ImageCompressor>) {
        self.compressors.push(compressor);
    }

    /// 按序执行压缩链，任一失败则降级返回原始数据
    pub fn run(&self, data: &[u8], media_type: &str) -> Vec<u8> {
        let mut current = data.to_vec();
        for c in &self.compressors {
            match c.compress(&current, media_type) {
                Ok(compressed) => current = compressed,
                Err(_) => return data.to_vec(), // 降级：返回原始数据
            }
        }
        current
    }

    /// 管线是否为空
    pub fn is_empty(&self) -> bool {
        self.compressors.is_empty()
    }
}

impl Default for CompressorPipeline {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 3: 创建 ImageMiddleware 主体**

创建 `peri-middlewares/src/middleware/image/mod.rs`：

```rust
mod compressor;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use peri_agent::{
    error::AgentResult,
    messages::{BaseMessage, ContentBlock, MessageContent},
    middleware::{r#trait::Middleware, state::MiddlewareState},
};
use regex::Regex;

pub use compressor::{CompressorPipeline, ImageCompressor};

/// 图片支持的 MIME 类型
const SUPPORTED_MIME: &[(&str, &str)] = &[
    ("image/png", ".png"),
    ("image/jpeg", ".jpg"),
    ("image/gif", ".gif"),
    ("image/webp", ".webp"),
];

/// ImageMiddleware — 解析用户消息中的 @image <path>，替换为 ContentBlock::Image
///
/// 在 `before_agent` 钩子中扫描最新一条 user message，查找 `@image <path>` 标记，
/// 读取对应图片文件，base64 编码后替换为 `ContentBlock::Image`。
/// 压缩管线为预留切面，MVP 为空——不对图片做任何压缩处理。
pub struct ImageMiddleware {
    image_dir: PathBuf,
    max_size: usize,
    compressors: CompressorPipeline,
}

impl ImageMiddleware {
    pub fn new(image_dir: PathBuf) -> Self {
        Self {
            image_dir,
            max_size: 20 * 1024 * 1024, // 默认 20MB 上限
            compressors: CompressorPipeline::new(),
        }
    }

    /// 设置最大文件大小（字节）
    pub fn with_max_size(mut self, max_size: usize) -> Self {
        self.max_size = max_size;
        self
    }

    /// 添加压缩器
    pub fn with_compressor(mut self, compressor: Box<dyn ImageCompressor>) -> Self {
        self.compressors.add(compressor);
        self
    }

    /// 确保图片目录存在
    fn ensure_image_dir(&self) -> Result<(), std::io::Error> {
        std::fs::create_dir_all(&self.image_dir)
    }
}

#[async_trait]
impl Middleware for ImageMiddleware {
    fn name(&self) -> &str {
        "ImageMiddleware"
    }

    async fn before_agent(&self, state: &mut dyn MiddlewareState) -> AgentResult<()> {
        // 取最后一条 Human 消息的索引
        let last_human_idx = state
            .messages()
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, m)| matches!(m, BaseMessage::Human { .. }).then_some(i));

        let idx = match last_human_idx {
            Some(i) => i,
            None => return Ok(()),
        };

        let text = state.messages()[idx].content();
        let re = match Regex::new(r"@image\s+(\S+)") {
            Ok(r) => r,
            Err(_) => return Ok(()),
        };

        // 收集所有 @image 路径
        let paths: Vec<String> = re
            .captures_iter(&text)
            .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
            .collect();

        if paths.is_empty() {
            return Ok(());
        }

        // 在 blocking 线程中批量处理（文件 I/O 可能阻塞）
        let max_size = self.max_size;
        let pipeline = &self.compressors;
        let results: Vec<Result<ContentBlock, String>> = tokio::task::spawn_blocking({
            let paths = paths.clone();
            move || {
                paths
                    .iter()
                    .map(|path| process_image_path(path, max_size, pipeline))
                    .collect()
            }
        })
        .await
        .map_err(|e| peri_agent::error::AgentError::MiddlewareError {
            middleware: "ImageMiddleware".to_string(),
            reason: format!("spawn_blocking 失败: {e}"),
        })?;

        // 重建 MessageContent：删除 @image 文本，追加 Image/Error 块
        let clean_text = re.replace_all(&text, "").to_string();
        let clean_text = clean_text.trim().to_string();

        let mut new_blocks: Vec<ContentBlock> = Vec::new();
        if !clean_text.is_empty() {
            new_blocks.push(ContentBlock::text(clean_text));
        }

        for result in &results {
            match result {
                Ok(block) => new_blocks.push(block.clone()),
                Err(err) => new_blocks.push(ContentBlock::text(format!("[{}]", err))),
            }
        }

        let msg_id = state.messages()[idx].id();
        let new_msg = state.messages()[idx].clone_with_content(MessageContent::Blocks(new_blocks));
        state.messages_mut()[idx] = new_msg;

        Ok(())
    }
}

/// 处理单张图片路径，成功返回 ContentBlock::Image，失败返回错误描述文本
fn process_image_path(
    raw_path: &str,
    max_size: usize,
    pipeline: &CompressorPipeline,
) -> Result<ContentBlock, String> {
    // 展开 ~ 和相对路径
    let expanded = shellexpand::tilde(raw_path).to_string();
    let path = Path::new(&expanded);

    if !path.exists() {
        return Err(format!("Image not found: {}", raw_path));
    }

    if !path.is_file() {
        return Err(format!("Not a file: {}", raw_path));
    }

    // 检查文件大小
    let metadata = std::fs::metadata(path).map_err(|e| format!("Cannot read file: {}", e))?;
    if metadata.len() > max_size as u64 {
        let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
        let max_mb = max_size as f64 / (1024.0 * 1024.0);
        return Err(format!(
            "Image too large: {:.1}MB > {:.0}MB limit",
            size_mb, max_mb
        ));
    }

    // 读取文件
    let data = std::fs::read(path).map_err(|e| format!("Cannot read file: {}", e))?;

    // MIME 检测
    let media_type = detect_mime(&data).unwrap_or("application/octet-stream");
    if !SUPPORTED_MIME.iter().any(|(mime, _)| *mime == media_type) {
        return Err(format!("Not an image: {}", raw_path));
    }

    // 压缩（MVP 空管线直接返回原数据）
    let processed = pipeline.run(&data, media_type);

    // base64 编码
    let base64_data = base64_encode(&processed);

    Ok(ContentBlock::image_base64(media_type, base64_data))
}

/// 使用 image crate 检测 MIME 类型
fn detect_mime(data: &[u8]) -> Option<&'static str> {
    use image::ImageFormat;
    let format = image::guess_format(data).ok()?;
    Some(match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Gif => "image/gif",
        ImageFormat::WebP => "image/webp",
        _ => return None,
    })
}

/// 标准 base64 编码（使用已有的 base64 crate）
fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}
```

- [ ] **Step 4: 编译验证**

```bash
cargo check -p peri-middlewares 2>&1
# 预期：编译通过
```

- [ ] **Step 5: 提交**

```bash
git add peri-middlewares/src/middleware/image/mod.rs peri-middlewares/src/middleware/image/compressor.rs peri-middlewares/Cargo.toml
git commit -m "feat(middleware): 新增 ImageMiddleware

在 before_agent 钩子中解析 @image <path> 标记，读取图片文件、
MIME 检测、大小校验后替换为 ContentBlock::Image。
压缩管线作为预留切面，MVP 为空。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 3: 注册 ImageMiddleware 到中间件链

**文件：**
- 修改：`peri-middlewares/src/middleware/mod.rs`
- 修改：`peri-acp/src/agent/builder.rs`

- [ ] **Step 1: 在 middlewares mod.rs 中注册**

在 `peri-middlewares/src/middleware/mod.rs` 中，紧跟现有 4 个中间件声明后添加：

```rust
pub mod image;
pub use image::ImageMiddleware;
```

完整修改后的 `mod.rs` 顶部应为：

```rust
pub mod filesystem;
pub mod terminal;
pub mod todo;
pub mod web;
pub mod image;        // 新增
pub use filesystem::FilesystemMiddleware;
pub use terminal::TerminalMiddleware;
pub use todo::TodoMiddleware;
pub use web::WebMiddleware;
pub use image::ImageMiddleware;  // 新增
```

- [ ] **Step 2: 在 ACP builder 中装配 ImageMiddleware**

在 `peri-acp/src/agent/builder.rs` 中，将 ImageMiddleware 插入到 `AtMentionMiddleware` 之后（两者都是用户消息内容处理类）：

读取 `builder.rs` 中 `AtMentionMiddleware` 的装配位置（约 493 行），在其后添加：

```rust
    chain.add(Box::new(peri_middlewares::AtMentionMiddleware::new(
        cwd.clone().into(),
    )));
    // 新增：图片附件处理（在 @mention 之后，将 @image <path> 转换为 ContentBlock::Image）
    chain.add(Box::new(peri_middlewares::ImageMiddleware::new()));
```

需要确认 `dirs_next` 已在 `peri-acp/Cargo.toml` 的依赖中（应与 workspace 一致 `dirs-next.workspace = true`）。

- [ ] **Step 3: 编译验证**

```bash
cargo check --workspace 2>&1
# 预期：全 workspace 编译通过
```

- [ ] **Step 4: 提交**

```bash
git add peri-middlewares/src/middleware/mod.rs peri-acp/src/agent/builder.rs
git commit -m "feat(acp): 装配 ImageMiddleware 到中间件链

ImageMiddleware 注册在 AtMentionMiddleware 之后，
在 before_agent 阶段将 @image <path> 转换为 ContentBlock::Image。

Co-Authored-By: deepseek-v4-pro <deepseek-ai@claude-code-best.win>"
```

---

### Task 4: 集成验证

- [ ] **Step 1: 运行全量测试确保无回归**

```bash
cargo test --workspace --lib 2>&1
# 预期：全部通过
```

- [ ] **Step 2: 运行 clippy 检查**

```bash
cargo clippy --workspace -- -D warnings 2>&1
# 预期：无新增 warning/error
```

- [ ] **Step 3: 手动验证清单**

- [ ] TUI 粘贴剪贴板图片后在输入框出现 `@image <path>` 文本
- [ ] 图片文件保存到 `~/.peri/images/` 且内容正确
- [ ] Agent 接收消息后 `ContentBlock::Image` 出现在 user message 中
- [ ] 不存在的路径显示 `[Image not found: ...]`
- [ ] 非图片文件显示 `[Not an image: ...]`
- [ ] 超限文件显示 `[Image too large: ...]`
- [ ] 现有测试不受影响

---

## Self-Review

### 1. Spec 覆盖检查

| Spec 需求 | 对应任务 |
|---|---|
| TUI 剪贴板图片粘贴 | Task 1 |
| ~/.peri/images/ 存储 | Task 1 (Step 3) |
| @image 语法 | Task 2 (regex + 解析) |
| before_agent 钩子处理 | Task 2 (Middleware impl) |
| MIME 检测 | Task 2 (detect_mime) |
| 大小校验 | Task 2 (process_image_path) |
| 压缩切面预留 | Task 2 (compressor.rs) |
| Provider 适配器 | 已有，不改动 ✓ |
| Compact 处理 | 已有，不改动 ✓ |
| 中间件链注册 | Task 3 |
| 边界情况（不存在/非图片/超限） | Task 2 (error 分支) |

### 2. Placeholder 扫描

- Task 2 中的 Middleware impl 代码需要进一步梳理——当前伪代码中使用了不存在的 compressors clone/borrow 操作。实际实现时 `CompressorPipeline` 应通过 `&self` 引用直接使用（不可变借用），不需要 clone 或 take。

### 3. 类型一致性

| 类型/函数 | 定义位置 | 使用位置 |
|---|---|---|
| `ImageMiddleware` | Task 2 (image/mod.rs) | Task 3 (builder.rs) |
| `CompressorPipeline` | Task 2 (compressor.rs) | Task 2 (image/mod.rs) |
| `ImageCompressor` trait | Task 2 (compressor.rs) | Task 2 (image/mod.rs::with_compressor) |
| `ContentBlock::image_base64` | 已有 (content.rs:246) | Task 2 (process_image_path) |
| `BaseMessage::clone_with_content` | 已有 (message.rs:243) | Task 2 (before_agent) |
| `MiddlewareState::messages_mut` | 已有 (state.rs:98) | Task 2 (before_agent) |
| `png_encode` | Task 1 (Step 4) | Task 1 (Step 3) |

### 4. 修正项

已在 Task 2 代码中直接修正——`CompressorPipeline` 通过 `&self` 不可变借用传递，使用 `tokio::task::spawn_blocking` 包装文件 I/O，命名采用 `pipeline` 而非 `compressors` 避免与 struct 字段混淆。
