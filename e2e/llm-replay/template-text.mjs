import { posix as path } from 'node:path';

const ENV_LINE_LABELS = { date: "Today's date: ", platform: 'Platform: ', os_version: 'OS Version: ' };

/** 只在系统环境段内、带固定标签且以换行结束的值可变；同一文本出现在用户消息或工具结果时不绑定。 */
export function environmentLine(text, offset, value, label) {
  const after = text[offset + value.length];
  return text.slice(Math.max(0, offset - label.length), offset) === label &&
    (!after || after === '\n' || after === '\r') &&
    text.lastIndexOf('<env>', offset) > text.lastIndexOf('</env>', offset) &&
    text.indexOf('</env>', offset + value.length) !== -1;
}

export function tokenBoundary(text, offset, binding, source, systemText) {
  const value = binding[source];
  const before = text[offset - 1] ?? '';
  const after = text[offset + value.length] ?? '';
  if (binding.kind === 'date') {
    // 日期只有系统环境快照中的这个带标签字段可变；用户日期和工具结果仍严格匹配。
    return systemText && environmentLine(text, offset, value, ENV_LINE_LABELS.date);
  }
  if (binding.kind === 'platform' || binding.kind === 'os_version') {
    // 宿主平台与补丁版本随 CI 镜像升级变化，不是业务行为；只归一化这两行，其余系统提示严格匹配。
    return systemText && environmentLine(text, offset, value, ENV_LINE_LABELS[binding.kind]);
  }
  if (binding.kind === 'path') {
    if ((before && /[\p{L}\p{N}_./~+%\-]/u.test(before)) || (after && after !== '/' && /[\p{L}\p{N}_.~+%\-]/u.test(after))) return false;
    if (after === '/') {
      const suffix = text.slice(offset + value.length).match(/^[^\s"'`<>()[\]{},;]+/u)?.[0] ?? '';
      if (path.normalize(value + suffix) !== value + suffix) return false;
    }
    return true;
  }
  // ID 必须是完整 token；bg-<uuid> 等派生身份需要单独 typed 声明。
  return !(before && /[\p{L}\p{N}_-]/u.test(before)) && !(after && /[\p{L}\p{N}_-]/u.test(after));
}

export function templateFor(text, bindings, source, systemText) {
  const candidates = [];
  for (const binding of bindings) {
    const value = binding[source];
    if (typeof value !== 'string') continue;
    for (let at = text.indexOf(value); at !== -1; at = text.indexOf(value, at + value.length)) {
      if (tokenBoundary(text, at, binding, source, systemText)) candidates.push({ at, end: at + value.length, binding });
    }
  }
  candidates.sort((left, right) => left.at - right.at || right.end - left.end);
  const segments = [];
  let cursor = 0;
  let found = false;
  for (const candidate of candidates) {
    if (candidate.at < cursor) continue;
    if (candidate.at > cursor) segments.push(text.slice(cursor, candidate.at));
    segments.push({ kind: candidate.binding.kind, alias: candidate.binding.alias });
    cursor = candidate.end;
    found = true;
  }
  if (!found) return null;
  if (cursor < text.length) segments.push(text.slice(cursor));
  return segments;
}
