/**
 * 单语句边界检查。
 *
 * `@tursodatabase/database` 0.8.3 的 `prepare()` 只编译输入中的第一条语句，
 * 后面的 SQL 会被静默忽略；驱动没有暴露 tail/语句数量。这里做一层 SQLite
 * 词法边界检查：字符串、注释里的分号不算边界，触发器的 BEGIN…END 体算一条语句。
 */

type TokenKind = 'word' | 'string' | 'punct';

interface Token {
  kind: TokenKind;
  value: string;
  upper: string;
}

export class SingleStatementError extends Error {}

/** 驱动默认仍可编译的 VACUUM INTO 会写外部文件，按单文件边界拒绝。 */
export class ExternalFileTargetError extends Error {}

// SQLite 的空白与标识符判定按 ASCII + 非 ASCII 字节，而不是 JS 的 \s/\p{L}。
function isSqliteSpace(char: string): boolean {
  return char === ' ' || char === '\t' || char === '\n' || char === '\f' || char === '\r';
}

function isIdentifierStart(char: string): boolean {
  const code = char.charCodeAt(0);
  return code >= 0x80 || code === 95 || (code >= 65 && code <= 90) || (code >= 97 && code <= 122);
}

function isIdentifierPart(char: string): boolean {
  const code = char.charCodeAt(0);
  return isIdentifierStart(char) || (code >= 48 && code <= 57) || code === 36;
}

function tokenize(sql: string): Token[] {
  const tokens: Token[] = [];
  let index = 0;

  while (index < sql.length) {
    const char = sql[index];
    if (char === undefined) break;

    if (isSqliteSpace(char)) {
      index++;
      continue;
    }

    if (char === '-' && sql[index + 1] === '-') {
      index += 2;
      // SQLite 的行注释只由 LF 结束；裸 CR 仍属于注释内容。
      while (index < sql.length && sql[index] !== '\n') index++;
      continue;
    }

    if (char === '/' && sql[index + 1] === '*') {
      index += 2;
      while (index < sql.length && !(sql[index] === '*' && sql[index + 1] === '/')) index++;
      if (index < sql.length) index += 2;
      continue;
    }

    if (char === "'" || char === '"' || char === '`') {
      const quote = char;
      let end = index + 1;
      while (end < sql.length) {
        if (sql[end] === quote) {
          if (sql[end + 1] === quote) {
            end += 2;
            continue;
          }
          end++;
          break;
        }
        end++;
      }
      tokens.push({ kind: 'string', value: sql.slice(index, end), upper: '' });
      index = end;
      continue;
    }

    if (char === '[') {
      let end = index + 1;
      while (end < sql.length && sql[end] !== ']') end++;
      if (end < sql.length) end++;
      tokens.push({ kind: 'string', value: sql.slice(index, end), upper: '' });
      index = end;
      continue;
    }

    if (char === ';') {
      tokens.push({ kind: 'punct', value: ';', upper: ';' });
      index++;
      continue;
    }

    if (isIdentifierStart(char)) {
      let end = index + 1;
      while (end < sql.length && isIdentifierPart(sql[end] ?? '')) end++;
      const value = sql.slice(index, end);
      tokens.push({ kind: 'word', value, upper: value.toUpperCase() });
      index = end;
      continue;
    }

    tokens.push({ kind: 'punct', value: char, upper: char });
    index++;
  }

  return tokens;
}

function isTrigger(tokens: readonly Token[]): boolean {
  let index = 0;
  // EXPLAIN 前缀不改变语句边界，只影响执行方式。
  if (tokens[index]?.upper === 'EXPLAIN') {
    index++;
    if (tokens[index]?.upper === 'QUERY' && tokens[index + 1]?.upper === 'PLAN') index += 2;
  }
  if (tokens[index]?.upper !== 'CREATE') return false;
  index++;
  if (tokens[index]?.upper === 'TEMP' || tokens[index]?.upper === 'TEMPORARY') index++;
  return tokens[index]?.upper === 'TRIGGER';
}

function findTriggerEnd(tokens: readonly Token[]): number | undefined {
  let begin = -1;
  for (let index = 1; index < tokens.length; index++) {
    if (tokens[index]?.upper === 'BEGIN') {
      begin = index;
      break;
    }
  }
  if (begin < 0) return undefined;

  let caseDepth = 0;
  let bodyDepth = 1;
  for (let index = begin + 1; index < tokens.length; index++) {
    const upper = tokens[index]?.upper;
    if (upper === 'CASE') {
      caseDepth++;
    } else if (upper === 'BEGIN') {
      bodyDepth++;
    } else if (upper === 'END') {
      if (caseDepth > 0) {
        caseDepth--;
      } else {
        bodyDepth--;
        if (bodyDepth === 0) return index;
      }
    }
  }
  return undefined;
}

/**
 * 断言输入恰好包含一条语句。语法本身仍交给驱动校验；这里只拒绝可确认的后缀。
 */
export function assertSingleStatement(sql: string): void {
  const tokens = tokenize(sql);
  if (tokens.length === 0) throw new SingleStatementError('SQL 不能为空');

  if (!isTrigger(tokens)) {
    const semicolon = tokens.findIndex((token) => token.kind === 'punct' && token.value === ';');
    if (semicolon >= 0 && semicolon < tokens.length - 1) {
      throw new SingleStatementError('一次只能执行一条 SQL 语句');
    }
    return;
  }

  const end = findTriggerEnd(tokens);
  // 无法识别的触发器结构交给驱动报语法错误，避免把合法写法误判成多语句。
  if (end === undefined) return;

  let index = end + 1;
  if (tokens[index]?.kind === 'punct' && tokens[index]?.value === ';') index++;
  if (index < tokens.length) throw new SingleStatementError('一次只能执行一条 SQL 语句');
}

/**
 * 只拦截驱动默认仍可编译的 VACUUM INTO（会写外部文件）；EXPLAIN 不执行目标语句，
 * 因此保留只读执行计划。这里是单文件边界保护，不是通用 SQL 沙箱。
 */
export function assertNoExternalFileTarget(sql: string): void {
  const tokens = tokenize(sql);
  const first = tokens[0];
  if (first?.kind !== 'word' || first.upper !== 'VACUUM') return;
  if (tokens.some((token) => token.kind === 'word' && token.upper === 'INTO')) {
    throw new ExternalFileTargetError('不支持向外部文件导出（VACUUM INTO）');
  }
}
