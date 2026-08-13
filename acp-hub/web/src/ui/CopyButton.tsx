import { createSignal, onCleanup } from 'solid-js';
import { Button } from './Button';

export function CopyButton(props: { text: string; label?: string; copiedLabel?: string; class?: string; size?: 'compact' | 'default' }) {
  const [copied, setCopied] = createSignal(false);
  const [failed, setFailed] = createSignal(false);
  let timer: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => { if (timer) clearTimeout(timer); });
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(props.text);
      setFailed(false); setCopied(true);
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => setCopied(false), 1600);
    } catch { setCopied(false); setFailed(true); }
  };
  return <Button type="button" size={props.size} class={props.class} onClick={copy} aria-live="polite">{failed() ? '复制失败' : copied() ? (props.copiedLabel || '已复制') : (props.label || '复制')}</Button>;
}
