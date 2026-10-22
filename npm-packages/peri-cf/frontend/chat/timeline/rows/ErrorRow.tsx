import { CircleAlert } from 'lucide-react';

/** 行级错误：消息内容来自投影的公开错误，不补造原因。 */
export function ErrorRow({ message }: { message: string }) {
  return (
    <div
      className="text-destructive border-destructive/30 bg-destructive/5 flex items-start gap-2 rounded-lg border px-3 py-2 text-xs leading-5"
      role="alert"
      data-slot="peri-error-row"
    >
      <CircleAlert className="mt-0.5 size-3.5 shrink-0" aria-hidden="true" />
      <span className="min-w-0 [overflow-wrap:anywhere]">{message}</span>
    </div>
  );
}
