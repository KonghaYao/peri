export function netBalance(entries) {
  return entries.reduce((total, entry) => total + Math.abs(entry.amount), 0);
}
