#!/usr/bin/env bun
/**
 * check-coverage.mjs — 运行测试并断言覆盖率达标。
 *
 * bun 1.3 的 --coverage-threshold 不可用（bunfig coverageThreshold 未生效），
 * 这里解析 `bun test --coverage` 的 text 报告，按行覆盖率（Lines）断言阈值。
 * 不达标时 exit 1（CI 会失败）。
 */
import { spawnSync } from 'node:child_process'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const THRESHOLD = 80 // 行覆盖率 ≥ 80%（bun 表格输出本身即为百分比数值）

const pkgRoot = join(dirname(fileURLToPath(import.meta.url)), '..')

const r = spawnSync('bun', ['test', '--coverage'], {
  cwd: pkgRoot,
  encoding: 'utf8',
  stdio: ['ignore', 'pipe', 'pipe'],
})

const out = (r.stdout ?? '') + (r.stderr ?? '')
// bun 的 text 报告表格以 `|` 分隔列：`All files       |   83.27 |   94.39 |`
const m = out.match(/All files\s*\|\s*([\d.]+)\s*\|\s*([\d.]+)/)
const lines = m ? parseFloat(m[2]) : NaN

console.log(
  `\nCoverage: ${Number.isNaN(lines) ? '不可解析' : `${lines.toFixed(2)}%`} (阈值 ${THRESHOLD}%)`,
)

if (!Number.isNaN(lines) && lines < THRESHOLD) {
  console.error(`覆盖率不达标: ${lines.toFixed(2)}% < ${THRESHOLD}%`)
  process.exit(1)
}
if (r.status !== 0) {
  process.exit(r.status ?? 1)
}
if (Number.isNaN(lines)) {
  process.exit(1)
}
