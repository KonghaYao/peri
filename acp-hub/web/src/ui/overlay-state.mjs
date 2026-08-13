let activeOverlays = 0;
const targetCounts = new WeakMap();
const targetInitialInert = new WeakMap();
const stack = [];

export const acquireOverlay = (target) => {
  const token = Symbol('overlay');
  stack.push(token);
  const releaseInert = acquireInert(target);
  let released = false;
  return {
    isTop: () => stack.at(-1) === token,
    release: () => {
      if (released) return activeOverlays;
      released = true;
      const index = stack.lastIndexOf(token);
      if (index >= 0) stack.splice(index, 1);
      return releaseInert();
    },
  };
};

export const acquireInert = (target) => {
  activeOverlays += 1;
  if (target) {
    if (!targetCounts.has(target)) targetInitialInert.set(target, !!target.inert);
    targetCounts.set(target, (targetCounts.get(target) || 0) + 1);
    target.inert = true;
  }
  let released = false;
  return () => {
    if (released) return activeOverlays;
    released = true;
    activeOverlays = Math.max(0, activeOverlays - 1);
    if (target) {
      const remaining = Math.max(0, (targetCounts.get(target) || 1) - 1);
      if (remaining) targetCounts.set(target, remaining);
      else {
        targetCounts.delete(target);
        target.inert = targetInitialInert.get(target) || false;
        targetInitialInert.delete(target);
      }
    }
    return activeOverlays;
  };
};

export const activeOverlayCount = () => activeOverlays;
