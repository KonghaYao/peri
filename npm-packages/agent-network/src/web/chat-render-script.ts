/**
 * 消息渲染片段：时间胶囊、头像、群聊发送者、@ 高亮与回执。
 *
 * 与列表片段一样由 `chatClientScript` 拼进同一作用域；纯 DOM 构造，
 * 不把消息正文当 HTML 注入。
 */
export const CHAT_RENDER_SCRIPT = `
const TIME_GAP = 5 * 60 * 1000;

function senderName(senderId) {
  if (identity && senderId === identity.actor_id) return '我';
  const member = members.get(senderId);
  return member ? member.name : senderId;
}

/** 气泡时间胶囊：今天只显示时分，更早带日期。 */
function chatTime(value) {
  const ms = Number(value);
  if (!ms || Number.isNaN(ms)) return '';
  const date = new Date(ms);
  const now = new Date();
  const clock = clockTime(date);
  if (date.toDateString() === now.toDateString()) return clock;
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  if (date.toDateString() === yesterday.toDateString()) return '昨天 ' + clock;
  const day = (date.getMonth() + 1) + '月' + date.getDate() + '日';
  if (date.getFullYear() === now.getFullYear()) return day + ' ' + clock;
  return date.getFullYear() + '年' + day + ' ' + clock;
}

function fullTime(value) {
  const date = new Date(Number(value));
  return Number.isNaN(date.getTime()) ? '' : date.toLocaleString();
}

/**
 * 把 history_list 的 delivery_states 聚合成一条状态回执。
 *
 * 没有 delivery 表示消息只完成了广播（人类对人类的单聊、群聊未 @），
 * 显示为“已发送”；多 Agent 时按状态计数聚合，避免堆叠多行。
 */
function receiptText(states) {
  const pending = Number(states.pending || 0);
  const claimed = Number(states.claimed || 0);
  const completed = Number(states.completed || 0);
  const failed = Number(states.failed || 0);
  const cancelled = Number(states.cancelled || 0);
  const total = pending + claimed + completed + failed + cancelled;
  if (total === 0) return '已发送';
  if (total === 1) {
    if (pending === 1) return '已送达';
    if (claimed === 1) return 'Agent 已接收';
    if (completed === 1) return '已处理';
    if (failed === 1) return '发送失败';
    return '已取消';
  }
  const parts = [];
  if (pending > 0) parts.push(pending + '/' + total + ' 已送达');
  if (claimed > 0) parts.push(claimed + '/' + total + ' 处理中');
  if (completed > 0) parts.push(completed + ' 已处理');
  if (failed > 0) parts.push(failed + ' 发送失败');
  if (cancelled > 0) parts.push(cancelled + ' 已取消');
  return parts.join(' · ');
}

function mentionNames() {
  const names = [];
  for (const member of members.values()) {
    if (member.name) names.push(member.name);
  }
  if (identity && identity.name) names.push(identity.name);
  return names.sort(function (a, b) { return b.length - a.length; });
}

/** 纯文本写入，只把已知成员名对应的 @ 片段包成高亮，避免 HTML 注入。 */
function renderRichText(element, text) {
  element.replaceChildren();
  const names = mentionNames();
  if (names.length === 0) {
    element.textContent = text;
    return;
  }
  let index = 0;
  while (index < text.length) {
    const at = text.indexOf('@', index);
    if (at < 0) break;
    let matched = null;
    for (const name of names) {
      if (text.startsWith(name, at + 1)) {
        matched = name;
        break;
      }
    }
    if (!matched) {
      index = at + 1;
      continue;
    }
    if (at > index) element.append(document.createTextNode(text.slice(index, at)));
    const span = document.createElement('span');
    span.className = 'mention';
    span.textContent = '@' + matched;
    element.append(span);
    index = at + 1 + matched.length;
  }
  if (index < text.length) element.append(document.createTextNode(text.slice(index)));
}

function timeDivider(value) {
  const element = document.createElement('li');
  element.className = 'msg-time';
  const label = document.createElement('span');
  label.textContent = chatTime(value);
  element.append(label);
  return element;
}

function messageItem(item) {
  const mine = Boolean(identity && item.sender_id === identity.actor_id);
  const element = document.createElement('li');
  element.className = 'msg' + (mine ? ' mine' : '');
  const sender = senderName(item.sender_id);
  const avatar = avatarElement(mine && identity ? identity.name : sender, null, 'msg-avatar');
  const content = document.createElement('div');
  content.className = 'msg-content';
  if (!mine && room && room.kind === 'group') {
    const senderLabel = document.createElement('div');
    senderLabel.className = 'msg-sender';
    senderLabel.textContent = sender;
    content.append(senderLabel);
  }
  const bubble = document.createElement('div');
  bubble.className = 'bubble';
  bubble.title = fullTime(item.created_at);
  renderRichText(bubble, item.preview);
  content.append(bubble);
  if (item.body_bytes > new TextEncoder().encode(item.preview).length) {
    const expand = document.createElement('button');
    expand.type = 'button';
    expand.className = 'expand';
    expand.textContent = '展开全文';
    expand.addEventListener('click', async function () {
      expand.disabled = true;
      try {
        renderRichText(bubble, await readMessage(item.message_id));
        expand.remove();
      } catch (error) {
        expand.disabled = false;
        statusElement.textContent = '读取正文失败：' + error.message;
      }
    });
    content.append(expand);
  }
  if (mine) {
    const receipt = document.createElement('div');
    receipt.className = 'msg-receipt';
    receipt.textContent = receiptText(item.delivery_states || {});
    content.append(receipt);
  }
  element.append(avatar, content);
  return element;
}

/** 连续消息间隔超过 5 分钟时插入居中时间胶囊。 */
function appendHistory(container, items) {
  let previous = 0;
  for (const item of items) {
    const created = Number(item.created_at) || 0;
    if (!previous || created - previous > TIME_GAP) container.append(timeDivider(created));
    container.append(messageItem(item));
    previous = created;
  }
}

async function readMessage(messageId) {
  let chunk = await callTool('network_message_read', { message_id: messageId });
  let text = chunk.chunk;
  let cursor = chunk.next_cursor;
  for (let index = 0; index < MAX_CHUNKS && cursor && !chunk.eof; index += 1) {
    chunk = await callTool('network_message_read', {
      message_id: messageId,
      chunk_cursor: cursor
    });
    text += chunk.chunk;
    cursor = chunk.next_cursor;
  }
  if (cursor) text += '\\n…（消息过长，已截断）';
  return text;
}
`;
