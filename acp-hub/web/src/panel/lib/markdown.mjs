const SAFE_PROTOCOLS = new Set(['http:', 'https:', 'mailto:']);

export const safeHref = (value) => {
  const href = String(value || '').trim();
  if (!href || href.startsWith('//')) return null;
  try {
    const url = new URL(href, 'https://acp-hub.invalid/');
    if (!SAFE_PROTOCOLS.has(url.protocol)) return null;
    if (url.hostname === 'acp-hub.invalid' && !/^(?:https?:|mailto:)/i.test(href)) return null;
    return href;
  } catch {
    return null;
  }
};

const textToken = (text) => ({ type: 'text', text });

export const parseInline = (source) => {
  const input = String(source || '');
  const tokens = [];
  let cursor = 0;
  const pushText = (text) => {
    if (!text) return;
    const previous = tokens.at(-1);
    if (previous?.type === 'text') previous.text += text;
    else tokens.push(textToken(text));
  };

  while (cursor < input.length) {
    if (input[cursor] === '\\' && cursor + 1 < input.length && /[\\`*_[\]()#+.!>-]/.test(input[cursor + 1])) {
      pushText(input[cursor + 1]); cursor += 2; continue;
    }
    if (input[cursor] === '`') {
      const end = input.indexOf('`', cursor + 1);
      if (end > cursor + 1) { tokens.push({ type: 'code', text: input.slice(cursor + 1, end) }); cursor = end + 1; continue; }
    }
    if (input.startsWith('**', cursor)) {
      const end = input.indexOf('**', cursor + 2);
      if (end > cursor + 2) { tokens.push({ type: 'strong', children: parseInline(input.slice(cursor + 2, end)) }); cursor = end + 2; continue; }
    }
    if (input[cursor] === '*' || input[cursor] === '_') {
      const marker = input[cursor];
      const end = input.indexOf(marker, cursor + 1);
      if (end > cursor + 1) { tokens.push({ type: 'em', children: parseInline(input.slice(cursor + 1, end)) }); cursor = end + 1; continue; }
    }
    if (input[cursor] === '[') {
      const labelEnd = input.indexOf('](', cursor + 1);
      const hrefEnd = labelEnd >= 0 ? input.indexOf(')', labelEnd + 2) : -1;
      if (labelEnd > cursor + 1 && hrefEnd > labelEnd + 2) {
        const rawHref = input.slice(labelEnd + 2, hrefEnd);
        const href = safeHref(rawHref);
        const label = parseInline(input.slice(cursor + 1, labelEnd));
        if (href) tokens.push({ type: 'link', href, children: label });
        else tokens.push(...label, textToken(` (${rawHref})`));
        cursor = hrefEnd + 1; continue;
      }
    }
    const next = input.slice(cursor + 1).search(/[\\`*_[\]]/);
    const end = next < 0 ? input.length : cursor + 1 + next;
    pushText(input.slice(cursor, end));
    cursor = end;
  }
  return tokens;
};

const isBlockStart = (line) => /^\s*$|^ {0,3}(?:```|#{1,6}\s|>\s?|[-+*]\s+|\d+[.)]\s+|(?:---+|___+|\*\*\*+)\s*$)/.test(line);

export const parseMarkdown = (source) => {
  const lines = String(source || '').replace(/\r\n?/g, '\n').split('\n');
  const blocks = [];
  for (let i = 0; i < lines.length;) {
    const line = lines[i];
    if (!line.trim()) { i += 1; continue; }
    const fence = line.match(/^ {0,3}```([^`]*)$/);
    if (fence) {
      const content = []; i += 1;
      while (i < lines.length && !/^ {0,3}```\s*$/.test(lines[i])) content.push(lines[i++]);
      if (i < lines.length) i += 1;
      blocks.push({ type: 'code', language: (fence[1].trim().split(/\s+/, 1)[0] || '').replace(/[^a-zA-Z0-9#+._-]/g, '').slice(0, 32), text: content.join('\n') });
      continue;
    }
    const heading = line.match(/^ {0,3}(#{1,6})\s+(.+)$/);
    if (heading) { blocks.push({ type: 'heading', level: heading[1].length, children: parseInline(heading[2].replace(/\s+#+\s*$/, '')) }); i += 1; continue; }
    if (/^ {0,3}(?:---+|___+|\*\*\*+)\s*$/.test(line)) { blocks.push({ type: 'rule' }); i += 1; continue; }
    if (/^ {0,3}>/.test(line)) {
      const quote = [];
      while (i < lines.length && /^ {0,3}>/.test(lines[i])) quote.push(lines[i++].replace(/^ {0,3}>\s?/, ''));
      blocks.push({ type: 'quote', children: parseMarkdown(quote.join('\n')) }); continue;
    }
    const list = line.match(/^ {0,3}([-+*]|\d+[.)])\s+(.+)$/);
    if (list) {
      const ordered = /^\d/.test(list[1]); const items = [];
      while (i < lines.length) {
        const item = lines[i].match(/^ {0,3}([-+*]|\d+[.)])\s+(.+)$/);
        if (!item || /^\d/.test(item[1]) !== ordered) break;
        items.push(parseInline(item[2])); i += 1;
      }
      blocks.push({ type: 'list', ordered, items }); continue;
    }
    const paragraph = [line.trim()]; i += 1;
    while (i < lines.length && !isBlockStart(lines[i])) paragraph.push(lines[i++].trim());
    blocks.push({ type: 'paragraph', children: parseInline(paragraph.join(' ')) });
  }
  return blocks;
};
