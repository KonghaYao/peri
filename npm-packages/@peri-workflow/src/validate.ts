/**
 * workflow 脚本语法校验 — 供 `validate` CLI 子命令使用。
 *
 * 策略：引擎 `parseScript`（语法/export/import/meta 字面量检查）
 * + 静态补充检查，覆盖引擎查不出、只在运行时炸的常见错误：
 *   - `workflow.agent(...)` 旧式调用（引擎注入的是顶层自由函数）
 *   - 完全缺失 `export const meta`（宿主依赖 meta.name 展示 workflow_name）
 *   - 无 `return` 语句（脚本将返回 undefined）
 *
 * 本模块为纯函数（输入 source，输出结果），不触碰 fs/进程，便于单测。
 */
import * as engine from '@claude-code-best/workflow-engine'

export interface ValidationIssue {
  severity: 'error' | 'warning'
  message: string
}

export interface ValidationResult {
  ok: boolean
  /** 解析出的 meta；缺失时为 null */
  meta: { name?: string; description?: string } | null
  errors: ValidationIssue[]
  warnings: ValidationIssue[]
}

/** 旧式 `workflow.<fn>(` 调用 — 引擎注入的是顶层自由函数 agent/parallel/pipeline/phase/log */
const OLD_API_CALL = /\bworkflow\.(agent|parallel|pipeline|phase|log)\s*\(/g

/** 检测 body 是否包含 return（宽松：注释中的 return 会漏报 warning，但避免误报） */
const HAS_RETURN = /\breturn\b/

export function validateScript(source: string): ValidationResult {
  const errors: ValidationIssue[] = []
  const warnings: ValidationIssue[] = []

  let meta: { name?: string; description?: string } | null = null
  let body: string = source

  // 1. meta 提取。区分两种情况：
  //    - 返回 meta: null → 源码里根本没有 `export const meta`（真缺失，报错）
  //    - 抛 ScriptError → meta 存在但非法（字面量/字段/大括号），由 parseScript 报具体错误，这里不重复报
  try {
    const extracted = engine.extractMeta(source)
    meta = extracted.meta
    body = extracted.body
    if (!meta) {
      errors.push({
        severity: 'error',
        message:
          'workflow 脚本必须包含 export const meta = { name, description }（宿主依赖 meta.name 标识 workflow）。请补上 meta 声明。',
      })
    }
  } catch {
    // 交给 parseScript 报具体错误
  }

  // 2. 引擎级校验：语法 / 多余 export（含 export default）/ import / meta 字面量
  try {
    engine.parseScript(source)
  } catch (e) {
    errors.push({
      severity: 'error',
      message: e instanceof Error ? e.message : String(e),
    })
  }

  // 3. 静态补充检查
  for (const m of body.matchAll(OLD_API_CALL)) {
    errors.push({
      severity: 'error',
      message: `检测到旧式调用 workflow.${m[1]}(...)：引擎注入的是顶层自由函数，请改为直接调用 ${m[1]}(...)（无需 workflow. 前缀）。`,
    })
  }
  if (!HAS_RETURN.test(body)) {
    warnings.push({
      severity: 'warning',
      message:
        '未检测到 return 语句：脚本将返回 undefined。请在顶层用 return 返回结果（引擎只允许 export const meta，结果靠顶层 return 输出）。',
    })
  }

  return { ok: errors.length === 0, meta, errors, warnings }
}
