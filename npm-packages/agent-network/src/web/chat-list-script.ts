/**
 * 会话列表渲染片段。
 *
 * `chatClientScript` 把它拼进同一个 `<script>`，因此与 `chat-script.ts`
 * 共享作用域；拆文件只为控制单文件行数，不改变页面行为。
 */
export const CHAT_LIST_SCRIPT = `
let searchQuery = '';
let emptyListMessage = '';

const AVATAR_COLORS = ['#5b8def', '#e8a33d', '#6aa84f', '#d96459', '#8e7cc3', '#3d9a9a'];

function avatarColor(name) {
  const chars = Array.from(String(name || '?'));
  let hash = 0;
  for (const char of chars) hash = (hash * 31 + char.codePointAt(0)) % 9973;
  return AVATAR_COLORS[hash % AVATAR_COLORS.length];
}

function firstChar(name) {
  const chars = Array.from(String(name || '单聊').trim());
  return chars.length > 0 ? chars[0] : '聊';
}

/** 会话列表圆形头像；群聊为 2×2 首字色块，避免所有群共用同一个字。 */
function avatarElement(name, kind, className) {
  const element = document.createElement('span');
  element.className = (className || 'avatar') + (kind === 'group' ? ' avatar-grid' : '');
  element.setAttribute('aria-hidden', 'true');
  if (kind === 'group') {
    const chars = Array.from(String(name || '群聊').trim()).filter(function (char) {
      return char.trim().length > 0;
    });
    if (chars.length === 0) chars.push('群');
    for (let index = 0; index < 4; index += 1) {
      const cell = document.createElement('span');
      cell.className = 'avatar-cell';
      cell.textContent = chars[index % chars.length];
      cell.style.background = AVATAR_COLORS[index % AVATAR_COLORS.length];
      element.append(cell);
    }
  } else {
    element.textContent = firstChar(name);
    element.style.background = avatarColor(name);
  }
  return element;
}

function pad2(value) {
  return value < 10 ? '0' + value : String(value);
}

function clockTime(date) {
  return pad2(date.getHours()) + ':' + pad2(date.getMinutes());
}

/** 会话列表时间：今天显示时:分，昨天显示“昨天”，更早显示日期。 */
function relativeTime(value) {
  const ms = Number(value);
  if (!ms || Number.isNaN(ms)) return '';
  const date = new Date(ms);
  const now = new Date();
  if (date.toDateString() === now.toDateString()) return clockTime(date);
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  if (date.toDateString() === yesterday.toDateString()) return '昨天';
  const monthDay = (date.getMonth() + 1) + '/' + date.getDate();
  if (date.getFullYear() === now.getFullYear()) return monthDay;
  return date.getFullYear() + '/' + monthDay;
}

function messageTime(entry) {
  return entry.last_message ? Number(entry.last_message.created_at) : 0;
}

function sortRooms(entries) {
  return entries.slice().sort(function (a, b) {
    const byMessage = messageTime(b) - messageTime(a);
    if (byMessage !== 0) return byMessage;
    return Number(b.updated_at) - Number(a.updated_at);
  });
}

function setListHint(text) {
  if (!listStatusElement) return;
  listStatusElement.textContent = text;
  listStatusElement.hidden = !text;
}

function setEmptyListMessage(text) {
  emptyListMessage = text;
  setListHint(text);
}

function roomMatches(entry, query) {
  if (!query) return true;
  if (String(entry.name || '').toLowerCase().indexOf(query) >= 0) return true;
  const preview = entry.last_message ? entry.last_message.preview : '';
  return String(preview || '').toLowerCase().indexOf(query) >= 0;
}

function renderRooms() {
  if (!roomListElement) return;
  const query = searchQuery.trim().toLowerCase();
  const entries = query ? roomEntries.filter(function (entry) {
    return roomMatches(entry, query);
  }) : roomEntries;
  roomListElement.replaceChildren();
  for (const entry of entries) {
    const item = document.createElement('li');
    const link = document.createElement('a');
    link.className = 'room-item' + (entry.room_id === roomId ? ' active' : '');
    link.href = '/c/' + encodeURIComponent(entry.room_id);
    link.title = entry.name;
    if (entry.room_id === roomId) link.setAttribute('aria-current', 'page');
    const avatar = avatarElement(entry.name, entry.kind);
    const main = document.createElement('div');
    main.className = 'room-main';
    const top = document.createElement('div');
    top.className = 'room-top';
    const name = document.createElement('span');
    name.className = 'room-name';
    name.textContent = entry.name;
    const time = document.createElement('span');
    time.className = 'room-time';
    time.textContent = relativeTime(entry.last_message ? entry.last_message.created_at : entry.updated_at);
    top.append(name, time);
    const preview = document.createElement('div');
    preview.className = 'room-preview';
    preview.textContent = entry.last_message ? entry.last_message.preview : '还没有消息';
    main.append(top, preview);
    const avatarWrap = document.createElement('span');
    avatarWrap.className = 'avatar-wrap';
    avatarWrap.append(avatar);
    if (entry.my_last_read_seq !== entry.head_seq) {
      const dot = document.createElement('span');
      dot.className = 'unread';
      dot.setAttribute('aria-label', '未读');
      avatarWrap.append(dot);
    }
    link.append(avatarWrap, main);
    item.append(link);
    roomListElement.append(item);
  }
  if (query && entries.length === 0) {
    setListHint('没有匹配的会话');
  } else if (roomEntries.length === 0) {
    setListHint(emptyListMessage);
  } else {
    setListHint('');
  }
}

function updateRoomEntry(roomValue, lastItem, readSucceeded) {
  const entry = roomEntries.find(function (item) { return item.room_id === roomValue.room_id; });
  if (!entry) return;
  entry.head_seq = roomValue.head_seq;
  entry.updated_at = roomValue.updated_at;
  if (readSucceeded) entry.my_last_read_seq = roomValue.head_seq;
  if (lastItem) entry.last_message = lastItem;
  roomEntries = sortRooms(roomEntries);
  renderRooms();
}

async function loadRooms() {
  if (!browserToken()) {
    setEmptyListMessage('这个浏览器还没有身份。打开 Agent 分享链接，即可添加好友并开始单聊。');
    return;
  }
  try {
    const me = await ensureIdentity();
    if (accountNameElement) accountNameElement.textContent = me.name || '未命名';
    if (accountAvatarElement) {
      accountAvatarElement.textContent = firstChar(me.name);
      accountAvatarElement.style.background = avatarColor(me.name);
    }
    const page = await callTool('network_rooms_list', { limit: 50 });
    const entries = await Promise.all(page.items.map(async function (entry) {
      let name = entry.kind === 'direct' ? '单聊' : entry.title || '群聊';
      if (entry.kind === 'direct') {
        try {
          const detail = await callTool('network_room_get', { room_id: entry.room_id });
          const others = detail.members
            .filter(function (member) { return member.actor_id !== me.actor_id; })
            .map(function (member) { return member.name; });
          if (others.length > 0) name = others.join('、');
        } catch (error) {
          // 房间不可读时保留占位名称，列表仍可展示。
        }
      }
      return {
        room_id: entry.room_id,
        kind: entry.kind,
        name: name,
        head_seq: entry.head_seq,
        my_last_read_seq: entry.my_last_read_seq,
        last_message: entry.last_message,
        updated_at: entry.updated_at
      };
    }));
    roomEntries = sortRooms(entries);
    if (roomEntries.length === 0) {
      setEmptyListMessage('还没有单聊。打开 Agent 分享链接即可开始。');
    } else {
      setEmptyListMessage('');
    }
    renderRooms();
  } catch (error) {
    setEmptyListMessage('读取单聊失败：' + (error && error.message ? error.message : String(error)));
  }
}

if (searchInput) {
  searchInput.addEventListener('input', function () {
    searchQuery = searchInput.value;
    renderRooms();
  });
  searchInput.addEventListener('keydown', function (event) {
    if (event.key !== 'Escape' || !searchInput.value) return;
    searchInput.value = '';
    searchQuery = '';
    renderRooms();
  });
}
`;
