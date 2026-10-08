export function workerScheduler() {
  const timers = new Set<ReturnType<typeof setTimeout>>();
  let closed = false;
  return {
    schedule(callback: () => void, delay = 0): ReturnType<typeof setTimeout> {
      if (closed) throw new Error("WASM scheduler is closed");
      const timer = setTimeout(() => {
        timers.delete(timer);
        if (!closed) callback();
      }, delay);
      timers.add(timer);
      return timer;
    },
    cancel(timer: ReturnType<typeof setTimeout>): void {
      clearTimeout(timer);
      timers.delete(timer);
    },
    close(): void {
      closed = true;
      for (const timer of timers) clearTimeout(timer);
      timers.clear();
    },
  };
}
