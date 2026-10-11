import { useEffect, useRef, useState } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { ArrowDown, Check, CircleAlert, Copy, Puzzle, Sparkles, Square } from 'lucide-react';
import type { Entry, EntryBlock, Phase, ToolCall, ToolPayloadReader } from './types';
import { loadPayloadText } from './payloadReader';

function textOf(entry: Entry): string {
  return entry.blocks.flatMap((block) => (block.type === 'text' ? [block.text] : [])).join('');
}

function toolResultText(tool: ToolCall): string | null {
  const result = tool.result;
  if (result && typeof result === 'object' && typeof (result as Record<string, unknown>).contentText === 'string')
    return String((result as Record<string, unknown>).contentText);
  return null;
}

/** 工具卡片：状态、preview 与全文入口；晚到更新按 toolCallId 就地更新，不移动位置。 */
function ToolBlock({ block, reader }: { block: Extract<EntryBlock, { type: 'tool_call' }>; reader?: ToolPayloadReader }) {
  const [payload, setPayload] = useState<{ ok: boolean; text: string } | null>(null);
  const [loading, setLoading] = useState(false);
  const controller = useRef<AbortController | null>(null);
  useEffect(() => () => controller.current?.abort(), []);
  const tool = block.tool;
  const ref = tool?.resultRef ?? tool?.argumentsRef;
  const preview = tool?.resultRef?.preview ?? tool?.argumentsRef?.preview ?? (tool ? toolResultText(tool) : null);
  const load = async () => {
    if (!ref || loading) return;
    controller.current?.abort();
    const next = new AbortController();
    controller.current = next;
    setLoading(true);
    const result = await loadPayloadText(ref, reader, next.signal);
    if (next.signal.aborted) return;
    setLoading(false);
    setPayload({ ok: result.ok, text: result.ok ? result.text : result.error });
  };
  if (!tool) {
    return <div className="tool-card is-unavailable" role="status">
      <Puzzle size={15} />
      <div><strong>工具调用暂不可用</strong>
        <p>{block.unavailable === 'missing_tool' ? '引用的工具记录缺失，可能来自更早的版本。' : block.unavailable ?? '对象暂不可读。'}</p></div>
    </div>;
  }
  return <div className={`tool-card is-${tool.status}`}>
    <Puzzle size={15} />
    <div className="tool-card-body">
      <div className="tool-card-head"><strong>{tool.name}</strong><span className="tool-status">{tool.status}</span>
        {tool.kind && <span className="tool-kind">{tool.kind}</span>}</div>
      {preview && <p className="tool-preview">{preview}</p>}
      {ref && <button type="button" className="text-button" onClick={() => void load()} disabled={loading}>
        {loading ? '正在读取全文…' : '读取全文'}
      </button>}
      {payload && <pre className={`tool-payload ${payload.ok ? '' : 'has-error'}`}>{payload.text}</pre>}
      {tool.error && <p className="muted" role="status">{tool.error.message}</p>}
    </div>
  </div>;
}

function Block({ block, reader }: { block: EntryBlock; reader?: ToolPayloadReader }) {
  if (block.type === 'text') return <div className="markdown">
    <ReactMarkdown remarkPlugins={[remarkGfm]} skipHtml components={{
      a: ({ href, children }) => <a href={href} target="_blank" rel="noopener noreferrer">{children}</a>,
      img: ({ alt }) => <span className="image-placeholder">[图片：{alt || '未加载的图片'}]</span>,
      table: ({ children }) => <div className="table-scroll"><table>{children}</table></div>,
    }}>{block.text}</ReactMarkdown>
  </div>;
  if (block.type === 'reasoning') {
    if (block.visibility === 'hidden' || !block.text) return null;
    return <details className="reasoning-block"><summary>思考过程</summary><p>{block.text}</p></details>;
  }
  if (block.type === 'tool_call') return <ToolBlock block={block} reader={reader} />;
  // resource 等未核实来源的块按设计 4.2 明确展示不可读原因。
  return <div className="block-unavailable" role="status">
    <CircleAlert size={14} /><span>暂不支持的内容块（{block.reason}）</span>
  </div>;
}

function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  const [copyError, setCopyError] = useState(false);
  const timeout = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => () => { if (timeout.current) clearTimeout(timeout.current); }, []);
  if (!text) return null;
  return <div className="message-actions">
    <button type="button" className="icon-button" aria-label={copied ? '已复制回答' : '复制回答'} onClick={() => {
      void navigator.clipboard.writeText(text).then(() => {
        setCopied(true); setCopyError(false);
        if (timeout.current) clearTimeout(timeout.current);
        timeout.current = setTimeout(() => setCopied(false), 1800);
      }).catch(() => setCopyError(true));
    }}>{copied ? <Check size={16} /> : <Copy size={16} />}</button>
    <span role="status">{copied ? '已复制' : copyError ? '复制失败，请手动选择文本' : ''}</span>
  </div>;
}

function UserEntry({ entry }: { entry: Entry }) {
  return <article className="message user-message" aria-label="你的消息">
    <div className="user-body"><p>{textOf(entry)}</p></div>
  </article>;
}

function AssistantEntry({ entry, pending, reader }: { entry: Entry; pending: boolean; reader?: ToolPayloadReader }) {
  const failed = entry.status === 'error';
  const cancelled = entry.status === 'cancelled';
  const generating = pending && !failed && !cancelled && entry.status !== 'completed';
  const text = textOf(entry);
  const emptyCompleted = entry.blocks.length === 0 && entry.status === 'completed';
  return <article className="message assistant-message" aria-label="Peri 的回答">
    <div className="assistant-avatar"><Sparkles size={17} strokeWidth={1.7} /></div>
    <div className="assistant-body">
      <div className="message-author">Peri <span>思考伙伴</span></div>
      {entry.blocks.map((block) => <Block key={block.blockId} block={block} reader={reader} />)}
      {generating && entry.blocks.length === 0 && <div className="thinking" role="status"><span /><span /><span /><em>正在思考</em></div>}
      {(failed || cancelled) && <div className={`message-state ${failed ? 'has-error' : 'is-cancelled'}`} role="status">
        {failed ? <CircleAlert size={15} /> : <Square size={13} />}
        <div><strong>{failed ? '生成失败' : '已停止生成'}</strong>
          <p>{failed ? entry.error?.message?.trim() || '这次回答未能完成，请稍后重试。'
            : text ? '已保留停止前生成的内容。' : '本次生成已取消。'}</p></div>
      </div>}
      {emptyCompleted && !generating && !failed && !cancelled && <p className="muted">这条回答没有文本内容。</p>}
      {entry.status === 'running' && !generating && <p className="muted" role="status">生成尚未完成，请先确认服务端任务状态。</p>}
      {!generating && <CopyButton text={text} />}
    </div>
  </article>;
}

export function MessageList({ entries, phase, chatId, reader }: {
  entries: Entry[]; phase: Phase; chatId?: string; reader?: ToolPayloadReader;
}) {
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
  }, [entries, phase]);

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
      <div className="message-list">
        {entries.map((entry) => entry.role === 'user'
          ? <UserEntry key={entry.id} entry={entry} />
          : <AssistantEntry key={entry.id} entry={entry} pending={phase === 'streaming' || phase === 'cancelling'} reader={reader} />)}
      </div>
      {showJump && <button type="button" className="jump-button" onClick={jump} aria-label="回到最新消息"><ArrowDown size={16} /></button>}
    </div>
  );
}
