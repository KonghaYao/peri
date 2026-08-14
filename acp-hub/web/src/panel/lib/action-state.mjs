export const isTurnActive = (activeTurn) => {
  if (!activeTurn) return false;
  const status = String(activeTurn.turnStatus || '').toLowerCase();
  return !['completed', 'complete', 'interrupted', 'cancelled', 'canceled', 'failed', 'error', 'ended'].includes(status);
};
