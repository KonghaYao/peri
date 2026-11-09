/**
 * world:check 的录制侧交叉校验：cassette seal 的 requestMeta 记录录制时实际渲染的 leaf 哈希，
 * world seal 只能证明快照自洽，不能证明它仍与录制引用一致。未映射或缺失 leaf 的不一致无法由
 * world:sync 解释，必须作为 stale；映射 leaf 的差异只提示重录，由 verify 继续把关。
 */
import { readFile, readdir } from 'node:fs/promises';
import path from 'node:path';
import { sha256 } from '../llm-replay/matcher.mjs';
import { isSyncedWorldLeaf } from '../llm-replay/world-mapping.mjs';

/** 只读 seal，不加载请求正文；v1 cassette 没有 requestMeta，由 verify 的 legacy 门禁处理。 */
export async function readRecordedWorldRefs(fixturesDir) {
  const entries = await readdir(fixturesDir, { withFileTypes: true });
  const cassettes = [];
  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    if (!entry.isDirectory() || entry.name.startsWith('.') || entry.name === 'world') continue;
    const sealFile = path.join(fixturesDir, entry.name, 'cassette', 'seal.json');
    let seal;
    try { seal = JSON.parse(await readFile(sealFile, 'utf8')); }
    catch (error) {
      if (error.code === 'ENOENT') continue;
      throw new Error(`${entry.name}: cassette seal 无法读取或不是合法 JSON（${error.message}）`);
    }
    if (seal.requestMeta === undefined) continue;
    if (!seal.requestMeta || typeof seal.requestMeta !== 'object' || Array.isArray(seal.requestMeta)) {
      throw new Error(`${entry.name}: cassette seal requestMeta 结构无效`);
    }
    const refs = [];
    for (const [stub, meta] of Object.entries(seal.requestMeta)) {
      if (!meta || typeof meta !== 'object' || !Array.isArray(meta.worldRefs)) {
        throw new Error(`${entry.name}/${stub}: requestMeta.worldRefs 缺失`);
      }
      for (const ref of meta.worldRefs) {
        if (!ref || typeof ref.id !== 'string' || typeof ref.sha256 !== 'string') {
          throw new Error(`${entry.name}/${stub}: requestMeta.worldRefs 条目无效`);
        }
        refs.push({ id: ref.id, sha256: ref.sha256 });
      }
    }
    cassettes.push({ id: entry.name, refs });
  }
  return cassettes;
}

export function compareRecordedWorldRefs(world, cassettes) {
  const current = new Map();
  const drift = new Map();
  let references = 0;
  for (const cassette of cassettes) {
    for (const ref of cassette.refs) {
      references += 1;
      if (!current.has(ref.id)) {
        const bytes = world.leafBytes(ref.id);
        current.set(ref.id, bytes === undefined ? null : sha256(bytes));
      }
      const actual = current.get(ref.id);
      if (actual === ref.sha256) continue;
      if (!drift.has(ref.id)) {
        drift.set(ref.id, actual === null ? 'missing' : isSyncedWorldLeaf(ref.id) ? 'mapped' : 'unmapped');
      }
    }
  }
  const ids = (kind) => [...drift].filter(([, value]) => value === kind).map(([id]) => id).sort();
  return {
    cassettes: cassettes.length,
    references,
    mappedDrift: ids('mapped'),
    unmappedDrift: ids('unmapped'),
    missingLeaves: ids('missing'),
  };
}
