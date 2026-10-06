import { useEffect, useRef, useState } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { ArrowDown, Check, CircleAlert, Copy, Sparkles, Square } from 'lucide-react';
import type { Message, Phase } from './types';

function AssistantMessage({ message, pending }: { message: Message; pending: boolean }) {
  const failed = message.status === 'error' || (!message.status && Boolean(message.error));
  const cancelled = message.status === 'cancelled';
  const generating = pending && !failed && !cancelled && message.status !== 'completed';
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);
  const timeout = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (timeout.current) clearTimeout(timeout.current); }, []);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(message.content);
      setCopied(true);
      setCopyError(false);
      if (timeout.current) clearTimeout(timeout.current);
      timeout.current = setTimeout(() => setCopied(false), 1800);
    } catch { setCopyError(true); }
  };

  return (
    <article className="message assistant-message" aria-label="Peri 的回答">
      <div className="assistant-avatar"><Sparkles size={17} strokeWidth={1.7} /></div>
      <div className="assistant-body">
        <div className="message-author">Peri <span>思考伙伴</span></div>
        {message.content ? <div className="markdown">
          <ReactMarkdown remarkPlugins={[remarkGfm]} skipHtml components={{
            a: ({ href, children }) => <a href={href} target="_blank" rel="noopener noreferrer">{children}</a>,
            img: ({ alt }) => <span className="image-placeholder">[图片：{alt || '未加载的图片'}]</span>,
            table: ({ children }) => <div className="table-scroll"><table>{children}</table></div>,
          }}>{message.content}</ReactMarkdown>
        </div> : generating ? <div className="thinking" role="status"><span /><span /><span /><em>正在思考</em></div>
          : !failed && !cancelled && message.status !== 'running' ? <p className="muted">这条回答没有文本内容。</p> : null}
        {(failed || cancelled) && <div className={`message-state ${failed ? 'has-error' : 'is-cancelled'}`} role="status">
          {failed ? <CircleAlert size={15} /> : <Square size={13} />}
          <div><strong>{failed ? '生成失败' : '已停止生成'}</strong>
            <p>{failed ? message.error?.trim() || '这次回答未能完成，请稍后重试。'
              : message.content ? '已保留停止前生成的内容。' : '本次生成已取消。'}</p></div>
        </div>}
        {message.status === 'running' && !generating && <p className="muted" role="status">生成尚未完成，请先确认服务端任务状态。</p>}
        {!generating && message.content && <div className="message-actions">
          <button type="button" className="icon-button" onClick={() => void copy()} aria-label={copied ? '已复制回答' : '复制回答'}>
            {copied ? <Check size={16} /> : <Copy size={16} />}
          </button>
          <span role="status">{copied ? '已复制' : copyError ? '复制失败，请手动选择文本' : ''}</span>
        </div>}
      </div>
    </article>
  );
}

export function MessageList({ messages, phase, chatId }: { messages: Message[]; phase: Phase; chatId?: string }) {
  const viewport = useRef<HTMLDivElement>(null);
  const follow = useRef(true);
  const [showJump, setShowJump] = useState(false);

  useEffect(() => {
    follow.current = true;
    setShowJump(false);
  }, [chatId]);

  useEffect(() => {
    const container = viewport.current;
    if (container && follow.current) container.scrollTop = container.scrollHeight;
  }, [messages, phase]);

  const jump = () => {
    follow.current = true;
    setShowJump(false);
    viewport.current?.scrollTo({ top: viewport.current.scrollHeight, behavior: 'smooth' });
  };

  return (
    <div className="message-viewport" ref={viewport} onScroll={() => {
      const container = viewport.current;
      if (!container) return;
      follow.current = container.scrollHeight - container.scrollTop - container.clientHeight < 100;
      setShowJump(!follow.current);
    }}>
      <div className="message-column" role="log" aria-label="对话消息" aria-live="off">
        <div className="conversation-start">一段新的思考，从这里开始</div>
        {messages.map((message, index) => message.role === 'user'
          ? <article className="message user-message" key={message.id} aria-label="你的消息"><div>{message.content}</div></article>
          : <AssistantMessage key={message.id} message={message}
            pending={index === messages.length - 1 && ['streaming', 'cancelling', 'blocked'].includes(phase)} />)}
        <div className="stream-status" role="status">
          {phase === 'streaming' ? '正在生成回答…' : phase === 'cancelling' ? '正在等待服务端确认停止…'
            : phase === 'syncing' ? '正在同步会话历史…' : phase === 'blocked' ? '任务状态待确认，请点击停止' : ''}
        </div>
      </div>
      {showJump && <button className="jump-button" type="button" aria-label="跳到最新消息" onClick={jump}><ArrowDown size={18} /></button>}
    </div>
  );
}
