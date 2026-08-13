const validDate = (value) => {
  if (!value || value === '—') return null;
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? date : null;
};

export const messageTime = (value, now = Date.now()) => {
  const date = validDate(value);
  if (!date) return null;
  const current = new Date(now);
  const sameDay = date.getFullYear() === current.getFullYear()
    && date.getMonth() === current.getMonth()
    && date.getDate() === current.getDate();
  const exact = new Intl.DateTimeFormat('zh-CN', {
    year: 'numeric', month: 'long', day: 'numeric', hour: '2-digit', minute: '2-digit', second: '2-digit',
  }).format(date);
  if (sameDay) {
    return { label: new Intl.DateTimeFormat('zh-CN', { hour: '2-digit', minute: '2-digit' }).format(date), exact };
  }
  const elapsedDays = Math.floor((now - date.getTime()) / 86_400_000);
  if (elapsedDays >= 0 && elapsedDays < 7) {
    return { label: new Intl.DateTimeFormat('zh-CN', { weekday: 'short', hour: '2-digit', minute: '2-digit' }).format(date), exact };
  }
  return {
    label: new Intl.DateTimeFormat('zh-CN', {
      year: date.getFullYear() === current.getFullYear() ? undefined : 'numeric',
      month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit',
    }).format(date),
    exact,
  };
};
