// peri 图片渲染功能落地：spike → plan → code → review → fix → verify
// 依据文档：.peri/plans/image-rendering-research.md（两轮 advisor 审核定稿）
export const meta = {
  name: 'image-rendering-p0-p1',
  description: 'peri 图片渲染 P0/P1：3 spike 验证 → 规格 → 7 实现任务 → 并行 review → fix → verify',
}

const DOC = '/Users/konghayao/code/ai/perihelion/.peri/plans/image-rendering-research.md'
const SPEC = '/Users/konghayao/code/ai/perihelion/.peri/plans/image-p0-p1-spec.md'
const STDS = 'docs/standards/rust.md、docs/standards/tui.md、peri-tui/AGENTS.md（如存在需先读）'
const CWD = '/Users/konghayao/code/ai/perihelion'

const COMMON = `
工作目录 ${CWD}。中文汇报。**不要 git commit**。只改任务指定的文件，不碰无关代码（尤其不要改发送链路 peri-middlewares/src/middleware/image/mod.rs）。
先读邻近代码与适用 standard（${STDS}），遵循其风格与既有模式。所有结论/决策引用调研文档 ${DOC}。

`

// ================= Phase A: Spike 验证（并行 3 路） =================
phase('spike')
const [s1, s2, s3] = await parallel([
  // ---- S1: ConvertState 深度检查（只读） ----
  () => agent(`${COMMON}
任务 S1（只读，不写代码）：深度检查 peri 流式 markdown 增量缓存 ConvertState，回答图片语法接入的缓存兼容性问题。
读 ${DOC} 的 §3.3、§8.3（R3/spike 清单第 3 项），然后读：
- peri-tui/src/kit/markdown/convert.rs（完整，重点 rollback_trailing_unstable、block_line_ends、has_table_in_processed_blocks、has_potential_table_header）
- peri-tui/src/kit/markdown/mod.rs（parse_markdown_cached、sanitize/ensure_closed_code_fences 的位置与顺序、cache.stable_text 契约）
- peri-tui/src/kit/markdown/types.rs
必须回答：
1. 缓存键基于原始 markdown 还是 sanitized 后文本？starts_with 契约对"前置扫描+占位 token 替换"的兼容性（替换改变文本长度后 starts_with 是否仍成立）？
2. 尾部回滚粒度：能回滚到 block 边界（block_line_ends），能否覆盖"行内图片语法翻转"（未闭合时是普通文本，闭合后变图片）？段落级回滚是否足够？
3. 占位 token 替换发生在 sanitize 之前还是之后才对缓存契约最安全？
4. 已闭合段落（含已闭合图片语法）在后续追加时是否会被重复解析（性能/一致性）？
5. 若按"图片加入不稳定尾部判定"需要动哪些字段？
输出：结论写入 .peri/plans/spike-convert-state.md（中文，含 文件:行号 证据与对 T2/T3 的明确建议）。`, {
    label: 'spike:convert-state',
    allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'folder_operations'],
  }),

  // ---- S2: ratatui-image 最小 spike（写实验 crate） ----
  () => agent(`${COMMON}
任务 S2（可写代码，仅限临时实验 crate，不动主 workspace）：验证 ratatui-image 11.x 作为 P1 基础的可行性（${DOC} §8.3 spike 清单第 1/2 项）。
步骤：
1. 创建独立实验 crate side-projects/image-spike/（Cargo.toml 含空 [workspace] 段脱离主 workspace；依赖 ratatui 0.30.2 + ratatui-image，features 先查 crates.io 确认：需 default-features=false 关闭 chafa-dyn，保留 crossterm backend 与 image-defaults；必要时 WebSearch/WebFetch crates.io 与 github.com/ratatui/ratatui-image 确认 feature 名与最新版本）。
2. 验证并记录：cargo tree 依赖树无 chafa-dyn/libchafa；编译通过。
3. 读 registry 中 ratatui-image 源码（src/protocol/kitty.rs、src/image.rs / widget 相关），确认：Kitty 首帧 transmit 是否仅一次（AtomicBool 机制）；跨帧状态持久化需要怎么持有（StatefulImage vs Image widget）；resize 是否阻塞重编码；删除/清理 placement 的 API；escape 如何进入 ratatui buffer（stdout 模型）。
4. 写一个最小测试（ratatui TestBackend 渲染 Image widget，捕获 buffer 输出），验证能否观察到 kitty escape（\x1b_G）与首帧仅传输一次的断言方式；若 TestBackend 不可行，说明原因并给出人工验证清单。
5. 结论：P1 采用 ratatui-image 是否可行；feature 清单；窄接口建议（模块名/公开函数）；state 持有方式。
输出：结论写入 .peri/plans/spike-ratatui-image.md（中文，含版本、feature 名、源码 行号 证据、Cargo.toml 示例）。实验 crate 保留（后续可删）。`, {
    label: 'spike:ratatui-image',
    allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'Edit', 'Bash', 'WebSearch', 'WebFetch', 'folder_operations'],
  }),

  // ---- S3: 前置扫描实验矩阵（写实验 crate） ----
  () => agent(`${COMMON}
任务 S3（可写代码，仅限临时实验 crate，不动主 workspace）：验证 markdown 前置扫描 + 占位 token 替换方案（${DOC} §3.3 与 §8.3 spike 清单第 4 项）。
步骤：
1. 创建独立实验 crate side-projects/md-scan-matrix/（Cargo.toml 含空 [workspace]；依赖 pulldown-cmark 0.12（包名 pulldown-cmark-012 不行，直接用 pulldown-cmark = "0.12"）+ ratatui-kit-markdown 0.3.0 + ratatui 0.30（rk_parse 输出含 Line<Span>））。
2. 实现实验函数：a) 用 Parser::into_offset_iter() 以 Options::all() - ENABLE_SMART_PUNCTUATION 枚举 Tag::Image（字节区间/alt/url/title）；b) 生成确定性占位 token（如 \\u{0}编号 包裹或罕见串，需满足"不与用户输入碰撞/非结构语法/可映射回原始区间"）；c) 替换后喂 ratatui_kit_markdown::parse_markdown，对比替换前后块结构。
3. 实验矩阵（每项断言替换后 ParsedBlock 结构与语义预期一致）：独立图片、段落内图片、多图片；图片在列表/引用/表格/强调/链接附近；alt 含强调/转义/括号/空文本；destination/title 含空格/转义/括号；流式序列 '!' → '![alt]' → '![alt](' → '![alt](url' → '![alt](url)'（每阶段分别替换+parse，检查一致性）；占位字符串出现在用户输入中的碰撞。
4. 结论：占位 token 方案是否破坏块结构；推荐 token 形式；流式各阶段的一致性与失效点；对 T2 扫描器/T3 渲染的明确建议。
输出：结论写入 .peri/plans/spike-scan-matrix.md（中文，含矩阵结果表、代码摘录）。实验 crate 保留。`, {
    label: 'spike:scan-matrix',
    allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'Edit', 'Bash', 'folder_operations'],
  }),
])

// ================= Phase B: Plan =================
phase('plan')
const planResult = await agent(`${COMMON}
任务 PLAN：产出 P0/P1 实施规格。先读：
- 调研文档 ${DOC}（重点 §3.2/§3.3/§3.4、§6.1/§6.4、§7、§8）
- 三份 spike 结论：.peri/plans/spike-convert-state.md、.peri/plans/spike-ratatui-image.md、.peri/plans/spike-scan-matrix.md
- 现有代码骨架：peri-tui/src/kit/terminal_caps.rs、peri-tui/src/kit/markdown/{mod,types,convert}.rs、peri-tui/src/kit/message_area/render.rs（TuiUserBubble 渲染路径）、peri-tui/src/kit/input_area.rs:442-519（粘贴图片）
输出：把以下 7 个实现任务细化写入 ${SPEC}（中文），每任务给出：目标、涉及文件（精确路径）、新增公共类型/函数签名建议、与既有设施的接缝（如 TerminalCaps 扩展字段、MarkdownSegment variant、render.rs 分发点、RENDER_HEARTBEAT/panel_mouse 设施）、验收标准（可测试断言级别）、风险与回退。
T1 TerminalCaps graphics 能力位：GraphicsProtocol 枚举（Kitty/ITerm2/None）+ detect_graphics_protocol（品牌映射：TERM_PROGRAM ∈ kitty/ghostty/wezterm/warp→Kitty；iTerm.app→ITerm2 保持 disabled 语义；tmux/ConPTY/未知→None）+ 环境变量 override/disable（如 PERI_IMAGE=off/kitty）——参照 grok protocol_for_brand 与 ${DOC} §6.3 E5，但实现必须独立。
T2 图片前置扫描器：markdown/ 新增模块，基于 S3 结论；API 建议 fn scan_images(sanitized: &str) -> Vec<ImageInfo> + 占位替换 fn replace_images(text, infos) -> (String, Vec<ImageInfo>)（side table 关联）；作用文本与 sanitize 顺序以 S1/S3 结论为准。
T3 Image segment + 渲染：types.rs MarkdownSegment::Image(ImageSegment{alt,url,is_remote,span..})；convert.rs 在 rk_parse 输出上把占位 token 替换为 Image segment；文本降级 [Image: alt] (url) / [Remote image: alt] (url) / [Image] (url)；流式缓存交互遵循 S1 结论。
T4 用户气泡 @image 行：message_area 渲染中识别 @image <path>（复用 emphasize_user_line 的 @mention 高亮机制），显示文件名+大小 meta（不解析像素），hover 显示绝对路径，点击 open（参数化 std::process::Command，禁止 shell 拼接）；受管理目录（~/.peri/images）与任意路径显示层级按 ${DOC} §6.1 Q6。
T5 安全层：新模块（如 kit/image_safety.rs）：canonicalize 路径分级（受管理目录/手工路径）、常规文件/扩展名/MIME 头校验、六项资源上限常量（字节/宽高/总像素/同屏数/累计缓存/解析时间）、控制字符过滤（展示字段进终端前）、URL scheme 分类（local/remote-http/https/dangerous）；被 T4/T7 复用。
T6 ratatui-image 集成：Cargo.toml 依赖（版本/features 按 S2 结论，default-features=false）；kit 窄接口模块（如 kit/image_preview.rs）封装协议检测+Image widget 构造+state 持有；全部走 T1 能力位门控。
T7 上下文 overlay 预览：基于 T6 接口；触发三态（cursor/focus/hover，参照现有 panel_mouse.rs/mouse_router.rs 设施）；几何布局（居中、边框、meta 行，参照 grok geometry.rs 思路）；仅受管理目录自动预览，手工路径文本降级；resize/隐藏/切换清理。
规格必须可执行：实现 agent 只读 ${SPEC} + ${DOC} 即可开工。`, {
  label: 'plan',
  allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'folder_operations'],
})

// ================= Phase C: Code P0 =================
phase('code-p0')
const t1 = await agent(`${COMMON}
任务 T1：实现 TerminalCaps graphics 能力位。先读 ${DOC}（§6.3 E5）、${SPEC} 的 T1 段、peri-tui/src/kit/terminal_caps.rs 全文件及其测试（若有 *_test.rs 或内联 #[cfg(test)]）。
实现：GraphicsProtocol 枚举 + detect 函数（品牌映射 + PERI_IMAGE 环境变量 override/disable）+ 单元测试（品牌矩阵、override、未知终端默认 None）。保持文件现有结构与风格（env 探测模式）。
验收：cargo test -p peri-tui --lib -- terminal_caps 通过；无 clippy 新警告。`, {
  label: 'code:caps',
  allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'Edit', 'Bash', 'folder_operations'],
})

const t2 = await agent(`${COMMON}
任务 T2：实现图片前置扫描器。先读 ${DOC} §3.3、${SPEC} T2 段、spike 结论 .peri/plans/spike-scan-matrix.md 与 .peri/plans/spike-convert-state.md、peri-tui/src/kit/markdown/mod.rs（sanitize 流程与 parse_markdown_cached）。
实现：markdown/ 新增模块（如 image_scanner.rs）：scan_images（pulldown-cmark-012 的 into_offset_iter，Options 与 ratatui-kit-markdown 0.3.0 保持一致：all() - ENABLE_SMART_PUNCTUATION）+ 占位替换函数（token 形式与 side table 遵循 S3 结论）+ 单测（覆盖 S3 矩阵的关键用例：段落内/列表/流式未闭合/空 alt/碰撞）。注意扫描作用文本与 sanitize 的顺序以 spike 结论为准。
验收：cargo test -p peri-tui --lib -- markdown 通过；无 clippy 新警告。`, {
  label: 'code:scanner',
  allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'Edit', 'Bash', 'folder_operations'],
})

const t5 = await agent(`${COMMON}
任务 T5：实现安全层模块。先读 ${DOC} §6.1 Q6、§6.2 风险 1-6、${SPEC} T5 段、peri-tui/src/kit/input_area.rs:442-519（受管理目录 ~/.peri/images 的生成规则）。
实现：新模块 kit/image_safety.rs：路径分级（canonicalize 后是否在受管理目录）、常规文件校验、扩展名/MIME 头校验（用已有 png 依赖读 IHDR 尺寸，JPEG/GIF/WebP 仅扩展名+magic bytes 或拒绝）、六项资源上限常量、控制字符过滤函数（用于展示字段）、URL scheme 分类。导出供 T4/T7 使用。配套单测（路径穿越、symlink、超大声明尺寸、控制字符、scheme 分类）。
验收：cargo test -p peri-tui --lib -- image_safety 通过；无 clippy 新警告。`, {
  label: 'code:safety',
  allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'Edit', 'Bash', 'folder_operations'],
})

const t3 = await agent(`${COMMON}
任务 T3：实现 Image segment 与渲染。先读 ${DOC} §3.3/§6.1 Q2（二轮修订后的文本降级规范）、${SPEC} T3 段、spike 结论（S1/S3）、peri-tui/src/kit/markdown/types.rs、convert.rs、mod.rs 全文件。
实现：types.rs 增 MarkdownSegment::Image(ImageSegment)；convert.rs 在 rk_parse 结果上把占位 token 对应的 Span 替换为 Image segment（token→原始区间映射来自 T2 的 side table，T2 已完成，直接读其代码）；渲染层文本降级输出 [Image: alt] (url) / [Remote image: alt] (url) / [Image] (url)（含控制字符过滤，复用 T5）；流式缓存交互遵循 S1 结论（图片闭合前不固化、闭合块进入稳定缓存）。
注意：依赖 T2 已完成的 image_scanner 模块与 T5 的过滤函数——先读它们的公共接口再实现。
验收：cargo test -p peri-tui --lib -- markdown 通过（含新增 segment 测试）；无 clippy 新警告。`, {
  label: 'code:segment',
  allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'Edit', 'Bash', 'folder_operations'],
})

const t4 = await agent(`${COMMON}
任务 T4：实现用户气泡 @image 行识别与交互。先读 ${DOC} §6.1 Q6、${SPEC} T4 段、peri-tui/src/kit/tui_render_unit.rs（TuiUserBubble.text 结构）、peri-tui/src/kit/message_area/render.rs（emphasize_user_line 与 @mention 高亮、UserBubble 渲染路径）、kit/image_safety.rs（T5 已完成，直接使用）。
实现：消息区 UserBubble 渲染时识别 @image <path> 行：显示「文件名 · 大小」meta（文件名/大小来自文件系统 stat + 文件名，不解析像素；受管理目录内 PNG 可用 T5 的 header 解析显示尺寸）；hover 时显示绝对路径（复用现有 hover 设施）；点击 open 用参数化 std::process::Command（open 命令），禁止 shell 拼接；路径分级显示策略遵循 SPEC。配套测试（解析、降级、命令参数化）。
验收：cargo test -p peri-tui --lib -- message_area 或相关测试通过；无 clippy 新警告。`, {
  label: 'code:bubble',
  allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'Edit', 'Bash', 'folder_operations'],
})

// ================= Phase D: Code P1 =================
phase('code-p1')
const t6 = await agent(`${COMMON}
任务 T6：集成 ratatui-image 并提供窄接口。先读 ${DOC} §8.1 R1、${SPEC} T6 段、spike 结论 .peri/plans/spike-ratatui-image.md（S2 已确认版本/features/state 持有方式）。
实现：peri-tui/Cargo.toml 添加 ratatui-image（版本与 features 严格按 S2 结论，default-features=false，确认无 chafa）；新模块 kit/image_preview.rs 封装窄接口：协议门控（复用 T1 的 detect_graphics_protocol）、Image widget 构造、StatefulImage 状态持有（跨帧持久化）、尺寸适配函数；全部走 T1 能力位。配套单测（协议门控逻辑可测部分）。
验收：cargo build -p peri-tui 通过；cargo tree -p peri-tui | grep -i chafa 无输出；无 clippy 新警告。`, {
  label: 'code:image-lib',
  allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'Edit', 'Bash', 'WebSearch', 'WebFetch', 'folder_operations'],
})

const t7 = await agent(`${COMMON}
任务 T7：实现上下文 overlay 预览。先读 ${DOC} §3.4/§6.1 Q4、${SPEC} T7 段、peri-tui/src/kit/image_preview.rs（T6 已完成）、panel_mouse.rs/mouse_router.rs（hover 设施）、message_area/render.rs（气泡渲染分发）、focus_router.rs（cursor/focus）。
实现：图片 chip/行上 cursor 聚焦或 hover 时绘制 overlay（居中、边框、meta 行：「Image: <文件名> · WxH · PNG」）；三态触发（cursor/focus/hover）；仅受管理目录图片自动像素预览，手工路径文本降级（T5 分级）；overlay 隐藏/切换/resize 时清理（含 ratatui-image state 复位）；窄接口只经 T6。配套可测逻辑单测（触发判定、几何、分级降级）。
验收：cargo build -p peri-tui 通过；无 clippy 新警告；相关单测通过。`, {
  label: 'code:overlay',
  allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'Edit', 'Bash', 'folder_operations'],
})

// ================= Phase E: Review（并行） =================
phase('review')
const [r1, r2] = await parallel([
  () => agent(`${COMMON}
任务 R1：评审 P0 实现。对照 ${DOC} §6.4 验证标准（P0 部分）、§8.3 spike 清单、${SPEC} 各任务验收标准，审查 git diff 中 P0 相关改动（markdown/{image_scanner,types,convert,mod}.rs、terminal_caps.rs、image_safety.rs、message_area 用户气泡路径）。
关注：a) 解析正确性（图片语法识别/占位替换/流式一致性/未闭合处理）；b) 缓存契约（ConvertState 回滚、starts_with）；c) 文本降级规范（[Image: alt] (url) 形态、控制字符）；d) 与既有风格/标准（docs/standards/rust.md）的一致性；e) 测试覆盖缺口。
输出：发现清单（严重度 P0/P1/P2 + 文件:行号 + 建议）写入 .peri/plans/review-p0.md（中文）。`, {
    label: 'review:p0',
    allowedTools: ['Read', 'Grep', 'Glob', 'Bash', 'folder_operations'],
  }),
  () => agent(`${COMMON}
任务 R2：评审 P1 集成与安全。对照 ${DOC} §6.2 新增风险 1-9、§6.4 验证标准（P1/安全部分）、§8.1 R1/R2、${SPEC} T5/T6/T7 验收，审查 git diff 中 P1 与安全相关改动（image_preview.rs、overlay、image_safety.rs、Cargo.toml 依赖、@image open 交互）。
关注：a) ratatui-image 接入（default-features、状态跨帧、resize 阻塞、清理）；b) 路径安全（分级、symlink/TOCTOU、受管理目录校验）、六项资源上限落实；c) 命令注入（open 参数化）；d) 控制字符/escape 注入；e) 远程 URL 不下载原则；f) 依赖树清洁（chafa 未引入）。
输出：发现清单（严重度 P0/P1/P2 + 文件:行号 + 建议）写入 .peri/plans/review-p1.md（中文）。`, {
    label: 'review:p1-security',
    allowedTools: ['Read', 'Grep', 'Glob', 'Bash', 'folder_operations'],
  }),
])

// ================= Phase F: Fix =================
phase('fix')
const fixResult = await agent(`${COMMON}
任务 F1：修复 review 发现的问题。先读两份评审报告 .peri/plans/review-p0.md 与 .peri/plans/review-p1.md（P0/P1 全部发现），逐条修复：
- P0 严重度问题必须修；P1 尽量修；P2 记录在回复中。
- 修复时保持代码风格一致、不扩大改动范围、不引入新依赖（除非评审明确要求且符合文档约束）。
- 每条修复在回复中列出：问题 → 修复方式 → 文件:行号。
修复后运行 cargo build -p peri-tui 与相关单测确认无回归。`, {
  label: 'fix',
  allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'Edit', 'Bash', 'folder_operations'],
})

// ================= Phase G: Verify =================
phase('verify')
const verifyResult = await agent(`${COMMON}
任务 V1：最终验证。在 ${CWD} 依次执行并记录结果（Bash 用较长 timeout）：
1. cargo build -p peri-tui（全 feature）
2. cargo clippy -p peri-tui --all-targets -- -D warnings（非常关键，必须无警告）
3. cargo test -p peri-tui --lib（至少跑完 markdown / terminal_caps / image_safety / message_area 相关用例）
4. cargo tree -p peri-tui -i ratatui-image 与 grep -i chafa 确认依赖树干净
5. 抽查 git diff --stat 确认改动集中在预期文件（应只涉及 peri-tui 与文档、side-projects spike；不得出现 peri-middlewares 发送链路改动、不得有 commit）
若 2/3 失败：修复小问题（编译错误/警告）后重跑；无法修复的记录下来。
输出：验证报告写入 .peri/plans/image-rendering-verify.md（中文）：每项命令结果、失败项与原因、最终结论（通过/有条件通过/不通过）。`, {
  label: 'verify',
  allowedTools: ['Read', 'Grep', 'Glob', 'Write', 'Edit', 'Bash', 'folder_operations'],
})

return {
  spike: { s1, s2, s3 },
  plan: planResult,
  code: { t1, t2, t3, t4, t5, t6, t7 },
  review: { r1, r2 },
  fix: fixResult,
  verify: verifyResult,
}
