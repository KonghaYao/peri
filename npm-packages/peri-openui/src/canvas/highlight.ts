// Vendored from peri-canvas@0.1.0 (packages/canvas/src/highlight.ts).
// License declared MIT in packages/canvas/package.json; no repository LICENSE file.
// See PROVENANCE.md. Internal use only; do not redistribute without legal review.
export type HighlightToken = {
  content: string;
  color?: string;
  fontStyle?: number;
  col: number;
};

const KEYWORDS = new Set([
  "break",
  "case",
  "catch",
  "class",
  "const",
  "continue",
  "debugger",
  "default",
  "delete",
  "do",
  "else",
  "export",
  "extends",
  "false",
  "finally",
  "for",
  "function",
  "if",
  "import",
  "in",
  "instanceof",
  "let",
  "new",
  "null",
  "return",
  "super",
  "switch",
  "this",
  "throw",
  "true",
  "try",
  "typeof",
  "undefined",
  "var",
  "void",
  "while",
  "with",
  "yield",
  "async",
  "await",
  "from",
  "of",
  "as",
  "type",
  "interface",
  "enum",
  "implements",
  "private",
  "protected",
  "public",
  "readonly",
  "static",
  "abstract",
  "def",
  "elif",
  "except",
  "lambda",
  "pass",
  "raise",
  "with",
  "None",
  "True",
  "False",
  "and",
  "or",
  "not",
  "fn",
  "mod",
  "pub",
  "use",
  "struct",
  "impl",
  "match",
  "mut",
  "func",
  "package",
  "defer",
  "go",
  "chan",
  "select",
  "range",
]);

export const LANGUAGE_BY_EXT: Record<string, string> = {
  ts: "typescript",
  tsx: "tsx",
  js: "javascript",
  jsx: "jsx",
  mjs: "javascript",
  cjs: "javascript",
  py: "python",
  rs: "rust",
  go: "go",
  json: "json",
  css: "css",
  scss: "css",
  html: "html",
  md: "markdown",
  sh: "shell",
  bash: "shell",
  zsh: "shell",
  yml: "yaml",
  yaml: "yaml",
};

export function inferLanguageFromPath(path: string): string | undefined {
  const base = path.split(/[\\/]/).pop() ?? path;
  const ext = base.includes(".") ? base.split(".").pop()?.toLowerCase() : undefined;
  if (!ext) return undefined;
  return LANGUAGE_BY_EXT[ext];
}

const LANGUAGE_ALIASES: Record<string, string> = {
  ts: "typescript",
  js: "javascript",
  py: "python",
  rs: "rust",
  md: "markdown",
  yml: "yaml",
};

export function normalizeLanguage(language?: string): string | undefined {
  if (!language) return undefined;
  const lower = language.toLowerCase();
  return LANGUAGE_ALIASES[lower] ?? lower;
}

export type HighlightColors = {
  keyword: string;
  string: string;
  comment: string;
  number: string;
  type: string;
  punct: string;
  text: string;
};

function push(
  tokens: HighlightToken[],
  content: string,
  col: number,
  color: string,
  fontStyle?: number,
) {
  if (!content) return;
  tokens.push({ content, col, color, fontStyle });
}

export function tokenizeLine(
  line: string,
  language: string | undefined,
  colors: HighlightColors,
): HighlightToken[] {
  if (!language || !line) {
    return [{ content: line, col: 0, color: colors.text }];
  }
  const tokens: HighlightToken[] = [];
  let i = 0;
  const hashComment = language === "python" || language === "shell" || language === "yaml";
  const lineComment = language !== "json" && !hashComment ? "//" : null;

  while (i < line.length) {
    const rest = line.slice(i);
    if (hashComment && rest.startsWith("#")) {
      push(tokens, rest, i, colors.comment, 2);
      break;
    }
    if (lineComment && rest.startsWith(lineComment)) {
      push(tokens, rest, i, colors.comment, 2);
      break;
    }
    if (rest.startsWith("/*")) {
      const end = rest.indexOf("*/", 2);
      const chunk = end === -1 ? rest : rest.slice(0, end + 2);
      push(tokens, chunk, i, colors.comment, 2);
      i += chunk.length;
      continue;
    }
    const stringMatch = rest.match(/^(['"`])(?:\\.|(?!\1).)*\1/);
    if (stringMatch) {
      push(tokens, stringMatch[0], i, colors.string);
      i += stringMatch[0].length;
      continue;
    }
    const numberMatch = rest.match(/^(0x[\da-fA-F]+|\d+(?:\.\d+)?(?:e[+-]?\d+)?)/);
    if (numberMatch) {
      push(tokens, numberMatch[0], i, colors.number);
      i += numberMatch[0].length;
      continue;
    }
    const identMatch = rest.match(/^[A-Za-z_$][\w$]*/);
    if (identMatch) {
      const ident = identMatch[0];
      const isType = /^[A-Z]/.test(ident) && ident.length > 1;
      const color = KEYWORDS.has(ident)
        ? colors.keyword
        : isType
          ? colors.type
          : colors.text;
      push(tokens, ident, i, color);
      i += ident.length;
      continue;
    }
    push(tokens, rest[0], i, colors.punct);
    i += 1;
  }
  return tokens.length ? tokens : [{ content: line, col: 0, color: colors.text }];
}
