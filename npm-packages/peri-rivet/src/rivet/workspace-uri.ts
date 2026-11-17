import { PeriError, type PeriErrorDetails } from '../contracts/errors.js';

/**
 * sandbox workspace 的规范定位符：`peri+sandbox://<machineId>/<runtimePath>`。
 *
 * 本模块只做文法与规范形式校验，不访问文件系统、不做路径归一化；`runtimePath`
 * 的归一化权威仍是 `@peri-code/sdk` 的 `normalizeAbsolutePath`。URI 只是 locator，
 * 不是 containment 证明。
 */

declare const workspaceUriBrand: unique symbol;

/** 规范 URI 文本；只能由 parse/format/canonicalize 产生。 */
export type WorkspaceUri = string & { readonly [workspaceUriBrand]: 'WorkspaceUri' };

/** 解析结果：`uri` 是规范文本，`runtimePath` 是同一路径的原始形态。 */
export interface WorkspaceComponents {
  readonly uri: WorkspaceUri;
  /** 规范小写 UUID。 */
  readonly machineId: string;
  /** 已规范化的绝对路径；Windows 写作 `c:/...`。 */
  readonly runtimePath: string;
}

export type WorkspaceUriErrorReason =
  | 'invalid_scheme'
  | 'missing_authority'
  | 'missing_machine'
  | 'invalid_machine'
  | 'machine_mismatch'
  | 'empty_path'
  | 'relative_path'
  | 'empty_segment'
  | 'trailing_separator'
  | 'dot_segment'
  | 'path_traversal'
  | 'separator_in_segment'
  | 'invalid_percent_encoding'
  | 'invalid_utf8'
  | 'non_canonical'
  | 'non_canonical_path'
  | 'ambiguous_path'
  | 'control_character'
  | 'query_or_fragment'
  | 'userinfo_or_port'
  | 'too_long';

export const WORKSPACE_URI_SCHEME = 'peri+sandbox';
export const MAX_WORKSPACE_URI_LENGTH = 2048;

const SCHEME_PREFIX = `${WORKSPACE_URI_SCHEME}://`;
const MACHINE_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const MACHINE_ID_ANY_CASE = /^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$/;
const RAW_SEGMENT_CHARS = "-._~!$&'()*+,;=:@";
const TEXT_ENCODER = new TextEncoder();
const TEXT_DECODER = new TextDecoder('utf-8', { fatal: true });

/** message 只写原因，不回显输入：URI/path 可能带凭据或可注入日志的字符。 */
function invalid(reason: WorkspaceUriErrorReason, extra?: PeriErrorDetails): never {
  throw new PeriError({
    code: 'invalid_identity',
    phase: 'input',
    message: `workspace URI ${reason}`,
    details: { reason, field: 'workspaceUri', ...extra },
  });
}

function hasControl(value: string): boolean {
  for (const character of value) {
    const codePoint = character.codePointAt(0);
    if (codePoint !== undefined && (codePoint <= 0x1f || (codePoint >= 0x7f && codePoint <= 0x9f))) {
      return true;
    }
  }
  return false;
}

function isRawSegmentChar(codePoint: number): boolean {
  if (codePoint < 0x80) {
    const character = String.fromCharCode(codePoint);
    if (/[A-Za-z0-9]/.test(character) || RAW_SEGMENT_CHARS.includes(character)) {
      return true;
    }
  }
  return false;
}

function encodeSegment(value: string): string {
  let encoded = '';
  for (const character of value) {
    const codePoint = character.codePointAt(0);
    if (codePoint !== undefined && isRawSegmentChar(codePoint)) {
      encoded += character;
      continue;
    }
    for (const byte of TEXT_ENCODER.encode(character)) {
      encoded += `%${byte.toString(16).toUpperCase().padStart(2, '0')}`;
    }
  }
  return encoded;
}

/** 先在 raw 上按 `/` 切分，再对每段解码一次；绝不先整体解码再切分。 */
function decodeSegment(raw: string, canonicalize: boolean): string {
  const bytes: number[] = [];
  let index = 0;
  while (index < raw.length) {
    const character = raw.charAt(index);
    if (character === '%') {
      const hex = raw.slice(index + 1, index + 3);
      if (!/^[0-9a-fA-F]{2}$/.test(hex)) {
        invalid('invalid_percent_encoding');
      }
      bytes.push(Number.parseInt(hex, 16));
      index += 3;
      continue;
    }
    const codePoint = raw.codePointAt(index);
    if (codePoint === undefined) {
      invalid('invalid_utf8');
    }
    if (codePoint >= 0xd800 && codePoint <= 0xdfff) {
      invalid('invalid_utf8');
    }
    if (codePoint < 0x80) {
      bytes.push(codePoint);
      index += 1;
      continue;
    }
    // 严格 parse 只接受 ASCII；canonicalize 把 raw Unicode 归一成 UTF-8 转义。
    if (!canonicalize) {
      invalid('non_canonical');
    }
    const text = String.fromCodePoint(codePoint);
    bytes.push(...TEXT_ENCODER.encode(text));
    index += text.length;
  }

  let decoded: string;
  try {
    decoded = TEXT_DECODER.decode(Uint8Array.from(bytes));
  } catch {
    invalid('invalid_utf8');
  }
  if (hasControl(decoded)) {
    invalid('control_character');
  }
  return decoded;
}

function assertSegment(segment: string): void {
  if (segment.length === 0) {
    invalid('empty_segment');
  }
  if (segment.includes('/') || segment.includes('\\')) {
    invalid('separator_in_segment');
  }
  if (segment === '.') {
    invalid('dot_segment');
  }
  if (segment === '..') {
    invalid('path_traversal');
  }
}

interface ParsedPath {
  readonly runtimePath: string;
  readonly pathText: string;
}

/**
 * POSIX 根 `/` 是合法路径；Windows 盘符根必须写作 `/c:/`，其余尾斜杠一律拒绝。
 * 路径语义（`.`/`..`/`//`/盘符大小写）不在本模块修正。
 */
function parsePath(rawPath: string, canonicalize: boolean): ParsedPath {
  if (rawPath.includes('?') || rawPath.includes('#')) {
    invalid('query_or_fragment');
  }
  if (rawPath === '/') {
    return { runtimePath: '/', pathText: '/' };
  }

  const rawSegments = rawPath.slice(1).split('/');
  const hasTrailingSlash = rawSegments[rawSegments.length - 1] === '';
  if (hasTrailingSlash) {
    rawSegments.pop();
  }
  if (rawSegments.some((segment) => segment.length === 0)) {
    invalid('empty_segment');
  }

  const decoded = rawSegments.map((segment) => decodeSegment(segment, canonicalize));
  for (const segment of decoded) {
    assertSegment(segment);
  }

  const encoded = rawSegments.map((raw, index) => {
    const decodedSegment = decoded[index];
    if (decodedSegment === undefined) {
      invalid('non_canonical');
    }
    const canonical = encodeSegment(decodedSegment);
    if (!canonicalize && canonical !== raw) {
      invalid('non_canonical');
    }
    return canonical;
  });

  const first = decoded[0];
  if (first === undefined) {
    invalid('empty_segment');
  }
  const drive = /^([a-z]):$/.exec(first);
  if (drive === null && /^[A-Z]:$/.test(first)) {
    invalid('non_canonical_path');
  }
  if (drive === null) {
    if (hasTrailingSlash) {
      invalid('trailing_separator');
    }
    return { runtimePath: `/${decoded.join('/')}`, pathText: `/${encoded.join('/')}` };
  }

  const driveLetter = drive[1];
  if (driveLetter === undefined) {
    invalid('non_canonical_path');
  }
  const rest = decoded.slice(1);
  if (rest.length === 0) {
    // 规范盘符根必须写作 `/c:/`；`/c:` 是同一个路径的另一种写法，严格拒绝。
    if (!hasTrailingSlash) {
      invalid('non_canonical_path');
    }
    return { runtimePath: `${driveLetter}:/`, pathText: `/${driveLetter}:/` };
  }
  if (hasTrailingSlash) {
    invalid('trailing_separator');
  }
  return {
    runtimePath: `${driveLetter}:/${rest.join('/')}`,
    pathText: `/${driveLetter}:/${encoded.slice(1).join('/')}`,
  };
}

function normalizeMachine(value: string): string {
  if (!MACHINE_ID_ANY_CASE.test(value)) {
    invalid('invalid_machine');
  }
  return value.toLowerCase();
}

function strictMachine(value: string): string {
  if (!MACHINE_ID.test(value)) {
    invalid('invalid_machine');
  }
  return value;
}

/** 严格：只接受规范小写 UUID；actor 定义期守卫与 parse 共用。 */
export function parseMachineId(value: string): string {
  if (!MACHINE_ID.test(value)) {
    invalid('invalid_machine', { field: 'machineId' });
  }
  return value;
}

function splitUri(value: string, canonicalize: boolean): { machineId: string; rawPath: string } {
  if (value.length > MAX_WORKSPACE_URI_LENGTH) {
    invalid('too_long', { length: value.length });
  }
  if (canonicalize) {
    if (!/^peri\+sandbox:\/\//i.test(value)) {
      invalid(/^peri\+sandbox:/i.test(value) ? 'missing_authority' : 'invalid_scheme');
    }
  } else if (!value.startsWith(SCHEME_PREFIX)) {
    invalid(value.startsWith(`${WORKSPACE_URI_SCHEME}:`) ? 'missing_authority' : 'invalid_scheme');
  }

  const rest = value.slice(SCHEME_PREFIX.length);
  const boundary = rest.search(/[/?#]/);
  const authority = boundary === -1 ? rest : rest.slice(0, boundary);
  if (authority.length === 0) {
    invalid('missing_machine');
  }
  if (boundary !== -1 && rest[boundary] !== '/') {
    invalid('query_or_fragment');
  }
  if (authority.includes('@') || authority.includes(':')) {
    invalid('userinfo_or_port');
  }

  const machineId = canonicalize ? normalizeMachine(authority) : strictMachine(authority);
  if (boundary === -1) {
    invalid('empty_path');
  }
  return { machineId, rawPath: rest.slice(boundary) };
}

/** 严格解析：只接受规范形式；任何非规范或歧义输入都抛 `PeriError`。 */
export function parseWorkspaceUri(value: string): Readonly<WorkspaceComponents> {
  const { machineId, rawPath } = splitUri(value, false);
  const { runtimePath, pathText } = parsePath(rawPath, false);
  const uri = `${SCHEME_PREFIX}${machineId}${pathText}`;
  if (uri !== value) {
    invalid('non_canonical');
  }
  return { uri: uri as WorkspaceUri, machineId, runtimePath };
}

function assertFormatSegments(segments: readonly string[]): void {
  for (const segment of segments) {
    if (segment.length === 0 || segment === '.' || segment === '..' || segment.includes('\\')) {
      invalid('non_canonical_path');
    }
  }
}

function formatPath(runtimePath: string): string {
  if (runtimePath.length === 0) {
    invalid('relative_path');
  }
  if (hasControl(runtimePath)) {
    invalid('control_character');
  }
  if (runtimePath === '/') {
    return '/';
  }

  const windowsAbsolute = /^[a-zA-Z]:[\\/]/.test(runtimePath);
  if (!runtimePath.startsWith('/') && !windowsAbsolute) {
    invalid('relative_path');
  }
  if (runtimePath.includes('\\')) {
    invalid('non_canonical_path');
  }

  if (windowsAbsolute) {
    const drive = runtimePath.charAt(0);
    if (drive !== drive.toLowerCase()) {
      invalid('non_canonical_path');
    }
    const rest = runtimePath.slice(3);
    if (rest.length === 0) {
      return `/${drive}:/`;
    }
    const segments = rest.split('/');
    assertFormatSegments(segments);
    return `/${drive}:/${segments.map(encodeSegment).join('/')}`;
  }

  const segments = runtimePath.slice(1).split('/');
  const first = segments[0];
  if (first === undefined) {
    invalid('relative_path');
  }
  if (/^[a-z]:$/.test(first)) {
    invalid('ambiguous_path');
  }
  if (/^[A-Z]:$/.test(first)) {
    invalid('non_canonical_path');
  }
  assertFormatSegments(segments);
  return `/${segments.map(encodeSegment).join('/')}`;
}

/**
 * 从部件生成规范 URI。
 *
 * `machineId` 接受任意大小写并输出小写（format 的产物必须规范）；`parse` 仍只接受小写。
 * `runtimePath` 必须是规范绝对路径，本函数只做百分号编码，不修 `.`/`..`/`//`/尾斜杠。
 */
export function formatWorkspaceUri(parts: {
  readonly machineId: string;
  readonly runtimePath: string;
}): WorkspaceUri {
  const machineId = normalizeMachine(parts.machineId);
  const pathText = formatPath(parts.runtimePath);
  const uri = `${SCHEME_PREFIX}${machineId}${pathText}`;
  if (uri.length > MAX_WORKSPACE_URI_LENGTH) {
    invalid('too_long', { length: uri.length });
  }
  return uri as WorkspaceUri;
}

/**
 * workspace URI 值对象：构造时经 `formatWorkspaceUri` 校验并生成规范文本，
 * 与入参对象脱钩；跨边界仍以 `toString()` 的字符串传递。
 */
export class WorkspaceURI {
  readonly #uri: WorkspaceUri;

  constructor(parts: { readonly machineId: string; readonly runtimePath: string }) {
    this.#uri = formatWorkspaceUri(parts);
  }

  toString(): WorkspaceUri {
    return this.#uri;
  }
}

/**
 * 迁移/人机输入用：只修正 scheme、UUID 与百分号编码的大小写和非规范编码，
 * 不修路径语义（`.`/`..`/`//`/尾 `/`/盘符大小写仍然拒绝）。
 */
export function canonicalizeWorkspaceUri(value: string): WorkspaceUri {
  const { machineId, rawPath } = splitUri(value, true);
  const { pathText } = parsePath(rawPath, true);
  const uri = `${SCHEME_PREFIX}${machineId}${pathText}`;
  if (uri.length > MAX_WORKSPACE_URI_LENGTH) {
    invalid('too_long', { length: uri.length });
  }
  return uri as WorkspaceUri;
}
