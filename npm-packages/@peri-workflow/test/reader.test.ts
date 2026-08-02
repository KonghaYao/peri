/**
 * reader 模块测试 — 占位符替换、时长格式化、journal/outputs 读取、根目录定位。
 *
 * 纯函数测试为主：IO 函数通过显式传参（runDir / startDir）驱动，
 * 不依赖 process.cwd() 的隐式状态。
 */
import { describe, expect, test } from 'bun:test'
import { mkdtempSync, mkdirSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import {
  findRunsRoot,
  fmtDuration,
  loadJournal,
  loadOutputs,
  replacePlaceholders,
} from '../src/reader'

function makeRunDir(): string {
  const base = mkdtempSync(join(tmpdir(), 'workflow-reader-'))
  const runDir = join(base, '.claude', 'workflow-runs', 'run-test')
  mkdirSync(runDir, { recursive: true })
  return runDir
}

// ─── replacePlaceholders ────────────────────────────────────

describe('replacePlaceholders', () => {
  const outputs = new Map<string, string>([
    ['fixSummaries', 'long summary text'],
    ['nested.deep', 'deep value'],
    ['arr.0', 'first item'],
  ])

  test('顶层占位符替换为 outputs 内容', () => {
    expect(replacePlaceholders('${fixSummaries}', outputs)).toBe('long summary text')
  })

  test('嵌套对象与点路径 label', () => {
    const v = { a: { b: '${nested.deep}' }, c: 42 }
    expect(replacePlaceholders(v, outputs)).toEqual({ a: { b: 'deep value' }, c: 42 })
  })

  test('数组元素与 [i] label', () => {
    const v = { list: ['${arr.0}', 'plain'] }
    expect(replacePlaceholders(v, outputs)).toEqual({ list: ['first item', 'plain'] })
  })

  test('无对应文件的占位符保留原样', () => {
    expect(replacePlaceholders('${missing.label}', outputs)).toBe('${missing.label}')
  })

  test('非纯占位符字符串不替换', () => {
    expect(replacePlaceholders('prefix ${fixSummaries} suffix', outputs)).toBe(
      'prefix ${fixSummaries} suffix'
    )
  })

  test('标量与 null 原样返回', () => {
    expect(replacePlaceholders(123, outputs)).toBe(123)
    expect(replacePlaceholders(null, outputs)).toBeNull()
    expect(replacePlaceholders(undefined, outputs)).toBeUndefined()
  })
})

// ─── fmtDuration ────────────────────────────────────────────

describe('fmtDuration', () => {
  test('毫秒级', () => {
    expect(fmtDuration('2026-08-02T00:00:00Z', '2026-08-02T00:00:00.500Z')).toBe('500ms')
  })

  test('秒级', () => {
    expect(fmtDuration('2026-08-02T00:00:00Z', '2026-08-02T00:00:12.345Z')).toBe('12.3s')
  })

  test('分秒级', () => {
    expect(fmtDuration('2026-08-02T00:00:00Z', '2026-08-02T00:11:19Z')).toBe('11m19s')
    expect(fmtDuration('2026-08-02T00:00:00Z', '2026-08-02T00:11:19.9Z')).toBe('11m20s')
  })

  test('缺失或非法输入', () => {
    expect(fmtDuration(undefined, '2026-08-02T00:00:00Z')).toBe('-')
    expect(fmtDuration('2026-08-02T00:00:00Z', undefined)).toBe('-')
    expect(fmtDuration('not-a-date', '2026-08-02T00:00:00Z')).toBe('-')
  })
})

// ─── loadJournal ────────────────────────────────────────────

describe('loadJournal', () => {
  test('解析条目并跳过空行/坏行', () => {
    const runDir = makeRunDir()
    writeFileSync(
      join(runDir, 'journal.jsonl'),
      [
        JSON.stringify({
          key: 'k2',
          seq: 2,
          result: { kind: 'ok', output: 'b', usage: { outputTokens: 30 }, tokenCount: 40, durationMs: 1500 },
        }),
        '',
        'this is not json',
        JSON.stringify({
          key: 'k1',
          seq: 1,
          result: { kind: 'dead', reason: 'killed' },
        }),
      ].join('\n')
    )
    const entries = loadJournal(runDir)
    expect(entries).toHaveLength(2)
    // 按 seq 升序
    expect(entries[0].seq).toBe(1)
    expect(entries[0].kind).toBe('dead')
    expect(entries[0].reason).toBe('killed')
    expect(entries[1].seq).toBe(2)
    expect(entries[1].tokens).toBe(40) // tokenCount 优先于 usage.outputTokens
    expect(entries[1].durationMs).toBe(1500)
  })

  test('无 journal 文件返回空数组', () => {
    const runDir = makeRunDir()
    expect(loadJournal(runDir)).toEqual([])
  })

  test('result 缺失的条目被跳过', () => {
    const runDir = makeRunDir()
    writeFileSync(join(runDir, 'journal.jsonl'), JSON.stringify({ key: 'k', seq: 1 }) + '\n')
    expect(loadJournal(runDir)).toEqual([])
  })
})

// ─── loadOutputs ────────────────────────────────────────────

describe('loadOutputs', () => {
  test('读取 outputs/*.txt 并按 label 建索引', () => {
    const runDir = makeRunDir()
    mkdirSync(join(runDir, 'outputs'))
    writeFileSync(join(runDir, 'outputs', 'a.b.txt'), 'hello')
    writeFileSync(join(runDir, 'outputs', 'c.txt'), 'world')
    writeFileSync(join(runDir, 'outputs', 'ignore.md'), 'not output')
    const out = loadOutputs(runDir)
    expect(out.get('a.b')).toBe('hello')
    expect(out.get('c')).toBe('world')
    expect(out.has('ignore.md')).toBe(false)
  })

  test('无 outputs 目录返回空 Map', () => {
    const runDir = makeRunDir()
    expect(loadOutputs(runDir).size).toBe(0)
  })
})

// ─── findRunsRoot ───────────────────────────────────────────

describe('findRunsRoot', () => {
  test('从子目录向上定位 workflow-runs 根', () => {
    const base = mkdtempSync(join(tmpdir(), 'workflow-root-'))
    const runsDir = join(base, '.claude', 'workflow-runs')
    mkdirSync(runsDir, { recursive: true })
    const deep = join(base, 'src', 'deep', 'deeper')
    mkdirSync(deep, { recursive: true })
    expect(findRunsRoot(deep)).toBe(runsDir)
    expect(findRunsRoot(base)).toBe(runsDir)
  })

  test('找不到时返回 null', () => {
    const base = mkdtempSync(join(tmpdir(), 'workflow-noroot-'))
    const deep = join(base, 'a', 'b')
    mkdirSync(deep, { recursive: true })
    expect(findRunsRoot(deep)).toBeNull()
  })
})
