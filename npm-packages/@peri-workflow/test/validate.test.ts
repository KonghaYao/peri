import { describe, expect, test } from 'bun:test'
import { validateScript } from '../src/validate'

const GOOD = `
export const meta = {
  name: 'demo',
  description: 'A demo workflow',
}

const r = await agent('hello', { agentType: 'web-researcher' })
return { answer: r }
`

describe('validateScript', () => {
  test('合法脚本：ok + meta 解析 + 无错误无警告', () => {
    const r = validateScript(GOOD)
    expect(r.ok).toBe(true)
    expect(r.meta).toEqual({ name: 'demo', description: 'A demo workflow' })
    expect(r.errors).toEqual([])
    expect(r.warnings).toEqual([])
  })

  test('export default：引擎报错', () => {
    const r = validateScript(
      `export const meta = { name: 'x', description: 'y' }
export default async function() { return 1 }`,
    )
    expect(r.ok).toBe(false)
    expect(r.errors.length).toBe(1)
    expect(r.errors[0].message).toContain('only one export const meta')
  })

  test('旧式 workflow.agent(...) 调用：静态检查报错并给出修复指引', () => {
    const src = `export const meta = { name: 'x', description: 'y' }
const r = await workflow.agent('hi')
return r`
    const r = validateScript(src)
    expect(r.ok).toBe(false)
    expect(r.errors.length).toBe(1)
    expect(r.errors[0].message).toContain('workflow.agent(')
    expect(r.errors[0].message).toContain('agent(')
  })

  test('旧式 workflow.parallel/log 调用同样报错', () => {
    const src = `export const meta = { name: 'x', description: 'y' }
await workflow.parallel([1, 2])
workflow.log('hi')
return 1`
    const r = validateScript(src)
    expect(r.ok).toBe(false)
    expect(r.errors.length).toBe(2)
    expect(r.errors[0].message).toContain('workflow.parallel(')
    expect(r.errors[1].message).toContain('workflow.log(')
  })

  test('完全缺失 export const meta：报错', () => {
    const src = `const r = await agent('hi')
return r`
    const r = validateScript(src)
    expect(r.ok).toBe(false)
    expect(r.meta).toBeNull()
    expect(r.errors[0].message).toContain('export const meta')
  })

  test('meta 缺少 description：引擎报错', () => {
    const r = validateScript(`export const meta = { name: 'x' }\nreturn 1`)
    expect(r.ok).toBe(false)
    expect(r.errors[0].message).toContain('name and description')
  })

  test('语法错误：引擎报错', () => {
    const r = validateScript(`export const meta = { name: 'x', description: 'y' }\nconst = broken`)
    expect(r.ok).toBe(false)
    expect(r.errors[0].message).toContain('Script syntax error')
  })

  test('import 语句：引擎报错', () => {
    const r = validateScript(`import fs from 'node:fs'\nexport const meta = { name: 'x', description: 'y' }\nreturn 1`)
    expect(r.ok).toBe(false)
    expect(r.errors[0].message).toContain('import is not supported')
  })

  test('无 return：warning 而非 error，ok 仍为 true', () => {
    const r = validateScript(`export const meta = { name: 'x', description: 'y' }\nconst r = await agent('hi')`)
    expect(r.ok).toBe(true)
    expect(r.warnings.length).toBe(1)
    expect(r.warnings[0].message).toContain('return')
  })

  test('meta 非字面量（引用变量）：引擎报错', () => {
    const r = validateScript(`const NAME = 'x'\nexport const meta = { name: NAME, description: 'y' }\nreturn 1`)
    expect(r.ok).toBe(false)
    expect(r.errors[0].message).toContain('plain literal')
  })

  test('多个错误同时存在：全部收集（meta 缺失 + workflow 旧式调用）', () => {
    const src = `const r = await workflow.agent('hi')\nreturn r`
    const r = validateScript(src)
    expect(r.ok).toBe(false)
    expect(r.errors.length).toBe(2)
    expect(r.meta).toBeNull()
  })
})
