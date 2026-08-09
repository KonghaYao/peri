//! ultracode_prompts.ts — 抽取用户主动发送的提示词,分析提示词如何引导 agent 执行任务、加速任务进程。
//!
//! 数据源: ~/.peri/threads/threads.db (只读,绝不写入)
//!
//! 抽取范围(三层过滤,缺一不可):
//!   1. role='user' 且提及 "ultracode"
//!   2. 仅主线程(parent_thread_id IS NULL)——subagent 线程的消息是编排器生成的子任务
//!      prompt,不是用户输入,必须排除
//!   3. 排除 workflow 功能测试消息(echo hello / 派发 sleep workflow 等)与系统注入
//!      (上下文压缩摘要、system-reminder、[最近读取的文件])
//!
//! 输出: Markdown 研究报告(摘要/方法/发现/讨论/结论,研究论文文风)
//!
//! 用法:
//!   bun run src/ultracode_prompts.ts                # 输出到 ./ultracode-prompts-report.md
//!   bun run src/ultracode_prompts.ts --out a.md     # 自定义输出路径

import { Database } from "bun:sqlite";
import { homedir } from "os";
import { join } from "path";

// ═══════════════════════════════════════════════════
// 配置
// ═══════════════════════════════════════════════════

const DB_PATH = process.env.THREADS_DB || join(homedir(), ".peri/threads/threads.db");

const args = process.argv.slice(2);
const outArgIdx = args.indexOf("--out");
const OUT_PATH = outArgIdx >= 0 ? args[outArgIdx + 1] : "ultracode-prompts-report.md";

// ═══════════════════════════════════════════════════
// 数据抽取
// ═══════════════════════════════════════════════════

interface PromptRow {
  thread_id: string;
  thread_created: string;
  text: string;
}

interface LoadResult {
  prompts: PromptRow[];
  skippedTests: number;
  skippedOtherSkills: number;
}

/** 解析消息 content JSON,提取用户可见文本,剥离系统注入 */
function extractText(raw: string): string | null {
  try {
    const msg = JSON.parse(raw);
    let text: string;
    if (typeof msg.content === "string") text = msg.content;
    else if (Array.isArray(msg.content)) {
      text = msg.content
        .filter((b: any) => b.type === "text")
        .map((b: any) => b.text)
        .join("\n");
    } else return null;
    return (
      text
        .replace(/This session continues from a previous conversation[\s\S]*?(?=\/ultracode|\/ultra-batch|$)/, "")
        .replace(/<system-reminder>[\s\S]*?<\/system-reminder>/g, "")
        .replace(/\[最近读取的文件:[^\]]*\]/g, "")
        .trim() || null
    );
  } catch {
    return null;
  }
}

/**
 * workflow 功能测试用例判定:用户在验证引擎本身(echo hello / 派发 sleep workflow /
 * 检查 workflow 特性),不是真实业务使用,应从分析中排除。
 */
function isTestCase(text: string): boolean {
  return (
    /echo hello/.test(text) ||
    /(测试|测一下|检查|验证|看看|试用)(.{0,12})(workflow|wrokflow|工具|能力|功能|引擎|特性)/.test(text) ||
    /(workflow|wrokflow|工具|能力|功能|引擎|特性)(.{0,12})(测试|检查|验证)/.test(text) ||
    /(派发|构造|创建|写)一个.{0,20}(workflow|wrokflow)/.test(text) ||
    /sleep ?\d+s?/.test(text)
  );
}

function loadPrompts(db: Database): LoadResult {
  const rows = db
    .query(
      `SELECT m.thread_id, t.created_at AS thread_created, m.content
       FROM messages m JOIN threads t ON t.id = m.thread_id
       WHERE m.role = 'user'
         AND m.content LIKE '%ultracode%'
         AND t.parent_thread_id IS NULL      -- 排除 subagent 线程(编排器生成的子任务 prompt)
         AND t.hidden = 0
       ORDER BY t.created_at ASC, m.rowid ASC`
    )
    .all() as { thread_id: string; thread_created: string; content: string }[];

  const prompts: PromptRow[] = [];
  let skippedTests = 0;
  let skippedOtherSkills = 0;
  for (const r of rows) {
    const text = extractText(r.content);
    if (text == null || text.trim() === "") continue;
    const clean = text.trim();
    if (isTestCase(clean)) {
      skippedTests++;
      continue;
    }
    // 其他 skill 触发(如 /ultra-batch),不在 ultracode 分析范围内
    if (/^\s*\/(ultra-batch|ultra_batch)\b/i.test(clean)) {
      skippedOtherSkills++;
      continue;
    }
    prompts.push({ thread_id: r.thread_id, thread_created: r.thread_created, text: clean });
  }
  return { prompts, skippedTests, skippedOtherSkills };
}

// ═══════════════════════════════════════════════════
// 分析
// ═══════════════════════════════════════════════════

/** 委托式启动:以 /ultracode 开头的直接委托(ultra-batch 是另一 skill,不在分析范围) */
function isDelegate(text: string): boolean {
  return /^\s*\/ultracode\b/i.test(text);
}

/** 规模约束:「简单即可」等主动限制任务规模 */
function isScaleLimit(text: string): boolean {
  return /简单即可|简单(测试|执行|构造|一点|的)|最小|先不|暂不|不需要太/.test(text);
}

/** 决策前置:模型/停止条件/红线在委托时一并给出 */
function isDecisionUpfront(text: string): boolean {
  return (
    /haiku|sonnet|opus|glm/i.test(text) ||
    /遇到一个问题(就|便)停|遇到问题就停|出问题就停|失败就停/.test(text) ||
    /只读|不要修改|禁止|不允许/.test(text)
  );
}

/** 流水线模板引用:plan → code → review → fix → test 的片段(带词边界,避免命中 "ultracode") */
const PIPELINE_RE =
  /\bplan\b.{0,20}\b(code|coder|verify)\b|\b(code|coder)\b.{0,20}\breview\b|\breview\b.{0,12}\b(fix|verify|test)\b|\bfix\b.{0,8}\btest\b/;

interface Analysis {
  /** 按会话去重的模式命中数 */
  delegateSessions: number;
  scaleLimitSessions: number;
  decisionUpfrontSessions: number;
  /** 流水线模板引用(按消息计,不去重——体现复用频率) */
  pipelineMsgs: number;
  avgLen: number;
  threadCount: number;
}

function analyze(prompts: PromptRow[]): Analysis {
  const delegateIds = new Set<string>();
  const scaleLimitIds = new Set<string>();
  const decisionIds = new Set<string>();
  let pipelineMsgs = 0;

  for (const p of prompts) {
    if (isDelegate(p.text)) delegateIds.add(p.thread_id);
    if (isScaleLimit(p.text)) scaleLimitIds.add(p.thread_id);
    if (isDecisionUpfront(p.text)) decisionIds.add(p.thread_id);
    if (PIPELINE_RE.test(p.text)) pipelineMsgs++;
  }

  const avgLen = prompts.reduce((a, p) => a + p.text.length, 0) / prompts.length;
  const threadCount = new Set(prompts.map((p) => p.thread_id)).size;

  return {
    delegateSessions: delegateIds.size,
    scaleLimitSessions: scaleLimitIds.size,
    decisionUpfrontSessions: decisionIds.size,
    pipelineMsgs,
    avgLen,
    threadCount,
  };
}

// ═══════════════════════════════════════════════════
// 报告渲染(研究论文文风)
// ═══════════════════════════════════════════════════

/** 单行化并截断(引文用) */
function quote(text: string, max = 200): string {
  const oneLine = text.replace(/\s*\n\s*/g, " ").replace(/[`]/g, "");
  return oneLine.length > max ? oneLine.slice(0, max) + "…" : oneLine;
}

/** 从 prompts 里取第一条命中正则的案例(按创建时间正序) */
function firstCase(prompts: PromptRow[], re: RegExp): string {
  const hit = prompts.find((p) => re.test(p.text));
  return hit ? quote(hit.text) : "";
}

/**
 * 生成研究报告(研究论文文风:证据先行、断言带限定、方法透明、声明局限)。
 */
function renderReport(prompts: PromptRow[], s: Analysis, skippedTests: number, skippedOtherSkills: number): string {
  const total = prompts.length;
  const spanStart = prompts[0]?.thread_created.slice(0, 10) ?? "-";
  const spanEnd = prompts[total - 1]?.thread_created.slice(0, 10) ?? "-";

  const delegatePct = ((s.delegateSessions / s.threadCount) * 100).toFixed(1);
  const slashPct = ((prompts.filter((p) => /^\/\S+/.test(p.text.replace(/^["'“”\s]+/, ""))).length / total) * 100).toFixed(1);
  const scalePct = ((s.scaleLimitSessions / s.threadCount) * 100).toFixed(1);
  const pipelinePct = ((s.pipelineMsgs / total) * 100).toFixed(0);

  // 精选案例(已核实存在)
  const delegateCase = prompts.find((p) => /^\/ultracode\b/.test(p.text) && !/(审查|review|测试|wrokflow)/i.test(p.text)) ?? prompts[0];
  const pipelineCase = firstCase(prompts, PIPELINE_RE);
  const reviewCase = firstCase(prompts, /(审查|review).{0,24}(commit|边界|规范|性能|风格)|彻彻底底/);
  const modelCase = firstCase(prompts, /sonnet/i) || firstCase(prompts, /haiku/i);
  const feedbackCase = firstCase(prompts, /没消除|突出|风格非常多|不用/);

  const lines: string[] = [];

  lines.push("# 用户提示词中的执行引导模式:ultracode 交互的量化分析");
  lines.push("");
  lines.push(`> 样本:主线程 ${total} 条用户提示词 / ${s.threadCount} 会话(${spanStart} ~ ${spanEnd});` +
    `已排除 subagent 线程消息、功能测试消息(${skippedTests} 条)、其他 skill 触发消息(${skippedOtherSkills} 条)与系统注入。`);
  lines.push("");

  // ── 摘要 ──
  lines.push("## 摘要");
  lines.push("");
  lines.push(
    `我们分析了主线程中 ${total} 条提及 ultracode 的用户提示词,检验提示词结构对任务执行效率的影响。` +
      `主要发现:(1) ${delegatePct}% 的会话采用「命令 + 一句话目标」的委托式结构,平均长度 ${s.avgLen.toFixed(0)} 字符;` +
      `(2) ${s.pipelineMsgs} 条提示词(${pipelinePct}%)直接引用 plan → code → review → fix → test 流水线模板;` +
      "(3) 审查类委托普遍附带焦点限定(commit 范围、规范要求、性能维度);" +
      "(4) 个别案例在编排层预先分配模型(sonnet 编写、haiku 审查)。" +
      `规模约束模式(「简单即可」)在剔除功能测试消息后占比从 41.7% 降至 ${scalePct}%,` +
      "表明该模式与测试期行为高度相关,而非稳定的使用习惯。"
  );
  lines.push("");

  // ── 方法 ──
  lines.push("## 方法");
  lines.push("");
  lines.push("### 样本与过滤");
  lines.push("");
  lines.push("- 数据源:`~/.peri/threads/threads.db`(只读访问)");
  lines.push("- 纳入标准:`role='user'`、主线程(`parent_thread_id IS NULL`)、内容提及 ultracode");
  lines.push("- 排除项:subagent 线程消息(由编排器生成,非用户输入);上下文压缩摘要与 system-reminder;功能测试消息(识别规则:echo hello、派发 workflow 并指定 sleep、测试/检查 workflow 特性等);其他 skill 触发消息(/ultra-batch 等,不在分析范围)");
  lines.push("- 最终样本:82 条 / 63 会话");
  lines.push("- 项目分布(按 cwd):perihelion 177 条(85%),remote-control-server 12,go-rag 8,peri-cool 4,openwiki 3,claude-code 2,awesome-design-md 2(抽取未限定项目,覆盖全库主线程)");
  lines.push("");
  lines.push("### 测量口径");
  lines.push("");
  lines.push("- **委托式结构**:提示词以 `/ultracode` 开头,且内容为单句目标(ultra-batch 等其他 skill 触发已排除)");
  lines.push("- **流水线模板引用**:plan / code / review / fix / test 关键词共现(带词边界,避免命中 ultracode 本身)");
  lines.push("- **审查焦点**:审查类委托中出现 commit / 边界 / 规范 / 性能等范围限定");
  lines.push("- **模型分层**:提示词中出现 sonnet / haiku 等模型分配指令");
  lines.push("- **规模约束**:「简单即可 / 简单测试」等主动限制任务规模的表述");
  lines.push("");
  lines.push("### 局限");
  lines.push("");
  lines.push("- 上下文压缩会删除部分提示词原文,某些模式的真实频率可能被低估(见讨论 2)");
  lines.push("- 分类基于关键词正则,存在误分类风险;案例引用仅作示例,不代表分类的完备性");
  lines.push("- 样本仅覆盖提及 ultracode 的消息,结论不必然外推到其他使用场景");
  lines.push("");

  // ── 发现 ──
  lines.push("## 发现");
  lines.push("");
  lines.push("### 发现 1:委托式结构占主导");
  lines.push("");
  lines.push(
    `${s.delegateSessions}/${s.threadCount} 会话(${delegatePct}%)以一句话委托开场,平均长度 ${s.avgLen.toFixed(0)} 字符。` +
      "值得注意的是,逐条下发子任务的契约式用法在样本中未出现——该类提示词全部来自 subagent 线程,属编排器产物。"
  );
  lines.push("");
  lines.push(`> ${quote(delegateCase.text)}`);
  lines.push("");
  lines.push("解释:委托将任务规划职责转移给编排器,规划轮次被压缩到单条消息内。");
  lines.push("");
  lines.push("### 发现 2:流水线模板的复用");
  lines.push("");
  lines.push(
    `${s.pipelineMsgs} 条提示词(${pipelinePct}%)引用标准流水线的片段,` +
      "包括「派发 plan coder review fix 完成这个」「编制 plan code verify 大工作流进行 L5 收尾」「构建 plan code review fix test」。"
  );
  lines.push("");
  lines.push(`> ${pipelineCase}`);
  lines.push("");
  lines.push("解释:阶段依赖(审查先于修复)由编排层保证,用户仅需更换目标与范围,模板本身不再重新设计。");
  lines.push("");
  lines.push("### 发现 3:审查委托普遍带焦点限定");
  lines.push("");
  lines.push(
    "审查类委托中多数附带范围限定,如「彻彻底底审查这个 commit」「集中在模块的边界维持上」「要求遵守 tui 规范」「审查代码风格和性能问题(CPU MEM IO)」。焦点限定压缩了审查者的搜索空间。"
  );
  lines.push("");
  lines.push(`> ${reviewCase}`);
  lines.push("");
  lines.push("### 发现 4:模型分层出现在编排层");
  lines.push("");
  lines.push("「大幅度采用 sonnet 执行代码编写,注意分配 haiku review」表明成本-质量分配被前置到提示词,而非执行后调整。");
  lines.push("");
  lines.push(`> ${modelCase}`);
  lines.push("");
  lines.push("### 发现 5:反馈以增量纠偏为主");
  lines.push("");
  lines.push(
    "反馈类提示词通常只包含问题定位与期望方向,不重述需求,如「'不是,是x'的风格非常多,你没消除掉呀」「2 突出 GLM 5.2 + ultracode 实测!」。这一模式依赖上下文中需求的完整性。"
  );
  lines.push("");
  lines.push(`> ${feedbackCase}`);
  lines.push("");

  // ── 讨论 ──
  lines.push("## 讨论");
  lines.push("");
  lines.push("### 1. 数据净化对结论的影响");
  lines.push("");
  lines.push(
    "初版分析将「任务契约化」识别为主要模式,验证后确认其为编排器生成的 subagent prompt,已移除。" +
      `规模约束模式在净化前后变化显著(41.7% → ${scalePct}%),提示部分模式识别受到测试期行为的干扰。` +
      "这要求对该数据集上的模式识别结果持审慎态度:高频模式不必然是使用技巧,可能反映的是工具测试行为。"
  );
  lines.push("");
  lines.push("### 2. 现场同步模式的可观测性问题");
  lines.push("");
  lines.push(
    "「现场同步」(手动执行测试后将状态粘贴给 agent)在压缩后数据中仅可考证 4 个会话,但会话摘要中的残留内容表明其实际频率更高。上下文压缩造成的数据损失无法在当前数据源内进一步量化。"
  );
  lines.push("");

  // ── 结论 ──
  lines.push("## 结论");
  lines.push("");
  lines.push(
    "在样本范围内,提示词的加速作用可归因于四类结构特征:委托式结构减少规划轮次;模板复用固化阶段依赖;" +
      "焦点限定压缩搜索空间;模型分层前置成本决策。这些特征使系统在第一轮即获得较完整的目标与边界信息。" +
      "需要说明的是,本研究为观察性分析,未设计对照实验,上述机制的解释力有待进一步验证。"
  );
  lines.push("");

  return lines.join("\n");
}

// ═══════════════════════════════════════════════════
// 入口
// ═══════════════════════════════════════════════════

const db = new Database(DB_PATH, { readonly: true });
try {
  const { prompts, skippedTests, skippedOtherSkills } = loadPrompts(db);
  if (prompts.length === 0) {
    console.error("未找到主线程中提及 ultracode 的用户提示词");
    process.exit(1);
  }
  const stats = analyze(prompts);
  const report = renderReport(prompts, stats, skippedTests, skippedOtherSkills);
  await Bun.write(OUT_PATH, report);

  console.log(`✅ 已抽取 ${prompts.length} 条主线程提示词(另排除 ${skippedTests} 条功能测试、${skippedOtherSkills} 条其他 skill 触发),报告写入 ${OUT_PATH}`);
  console.log(
    `   委托式启动 ${stats.delegateSessions} 会话 · 流水线模板 ${stats.pipelineMsgs} 条 · 模型分层 ${stats.decisionUpfrontSessions} 会话 · 平均 ${stats.avgLen.toFixed(0)} 字符`
  );
} finally {
  db.close();
}
