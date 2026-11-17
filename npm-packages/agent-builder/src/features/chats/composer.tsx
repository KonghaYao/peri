import { SEND_TEXT_MAX_LENGTH } from '@shared/chat';
import { useState } from 'react';
import { Button } from '@/components/ui/button';
import { Textarea } from '@/components/ui/textarea';

export interface ComposerProps {
  readonly disabled: boolean;
  readonly sending: boolean;
  readonly running: boolean;
  readonly cancelling: boolean;
  readonly onSend: (text: string) => void;
  readonly onCancel: () => void;
}

export function Composer({ disabled, sending, running, cancelling, onSend, onCancel }: ComposerProps) {
  const [text, setText] = useState('');
  const trimmed = text.trim();
  const canSend = !disabled && !sending && trimmed.length > 0;

  return (
    <form
      className="space-y-2 border-t pt-4"
      onSubmit={(event) => {
        event.preventDefault();
        if (!canSend) return;
        onSend(trimmed);
        setText('');
      }}
    >
      <Textarea
        aria-label="消息内容"
        placeholder={disabled ? '对话未就绪，暂时无法发送' : '输入消息，Enter 换行；点击发送'}
        value={text}
        maxLength={SEND_TEXT_MAX_LENGTH}
        rows={3}
        disabled={disabled}
        onChange={(event) => setText(event.target.value)}
      />
      <div className="flex items-center justify-between gap-3">
        <p className="text-xs text-muted-foreground">
          {text.length} / {SEND_TEXT_MAX_LENGTH}
        </p>
        <div className="flex items-center gap-2">
          {running ? (
            <Button type="button" variant="outline" disabled={cancelling} onClick={onCancel}>
              {cancelling ? '取消中…' : '停止'}
            </Button>
          ) : null}
          <Button type="submit" disabled={!canSend}>
            {sending ? '发送中…' : '发送'}
          </Button>
        </div>
      </div>
    </form>
  );
}
