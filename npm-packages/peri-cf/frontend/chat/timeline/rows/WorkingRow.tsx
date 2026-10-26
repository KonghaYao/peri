import { useEffect, useState } from 'react';

function formatElapsed(startedAt: string, now: number): string {
  const started = Date.parse(startedAt);
  if (Number.isNaN(started)) return '';
  const totalSeconds = Math.max(0, Math.floor((now - started) / 1000));
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${seconds}s`;
  return `${seconds}s`;
}

/** 运行中的计时行；开始时间缺失时只显示 Working…，不猜测耗时。 */
export function WorkingRow({ startedAt }: { startedAt: string | null }) {
  const [now, setNow] = useState(() => Date.now());
  const valid = startedAt !== null && !Number.isNaN(Date.parse(startedAt));

  useEffect(() => {
    setNow(Date.now());
    if (!valid) return;
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [startedAt, valid]);

  return (
    <div className="border-border/70 border-b py-2" data-slot="peri-working-row">
      <div className="text-muted-foreground flex h-6 items-baseline gap-2 px-1 text-sm leading-relaxed tabular-nums">
        <span className="shimmer motion-reduce:animate-none">
          {valid ? `Working for ${formatElapsed(startedAt, now)}` : 'Working…'}
        </span>
      </div>
    </div>
  );
}
