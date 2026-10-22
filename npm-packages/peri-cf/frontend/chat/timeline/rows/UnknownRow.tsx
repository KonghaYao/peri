import { CircleAlert } from 'lucide-react';

/** 不可读的内容块：保留原因，不静默丢弃。 */
export function UnknownRow({ reason }: { reason: string }) {
  return (
    <div
      className="text-muted-foreground my-1 flex items-center gap-1.5 text-xs"
      role="status"
      data-slot="peri-unknown-block"
    >
      <CircleAlert className="size-3.5 shrink-0" aria-hidden="true" />
      <span>暂不支持的内容块（{reason}）</span>
    </div>
  );
}
