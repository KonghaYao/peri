import { lazy, Suspense, useEffect, useRef, useState } from 'react';
import {
  Activity, ArrowRight, ArrowUp, BookOpen, ChevronDown, Code2, Feather, Menu, MessageSquare,
  PanelLeftClose, Plus, RefreshCw, Settings2, ShieldCheck, Sparkles, Square, X,
} from 'lucide-react';
import { Settings } from './settings/Settings';
import { useChat } from './chat/useChat';
import type { Chat } from './chat/types';

const MessageList = lazy(() => import('./chat/MessageList').then((module) => ({ default: module.MessageList })));
const InstancesPanel = lazy(() => import('./instances/InstancesPanel').then((module) => ({ default: module.InstancesPanel })));

const tokenKey = 'peri.access-token';
const suggestions = [
  { icon: Feather, title: '让文字更有力量', description: '写作、润色，让表达恰到好处', prompt: '帮我写一封简洁、真诚的工作邮件。请先问我收件人和想表达的内容。', color: 'peach' },
  { icon: Code2, title: '一起解决代码难题', description: '读懂代码，找到更好的解法', prompt: '我想请你帮我分析一段代码。请先问我使用的语言和遇到的问题。', color: 'blue' },
  { icon: BookOpen, title: '把复杂的事讲明白', description: '拆解知识，收获新的理解', prompt: '请用通俗的语言解释一个复杂概念。先问我想了解什么，以及我的知识背景。', color: 'lavender' },
  { icon: Sparkles, title: '给想法一点新灵感', description: '打开思路，发现更多可能', prompt: '我想和你一起头脑风暴。请先问我目标和限制条件，再帮我拓展思路。', color: 'green' },
];

function readToken(): string {
  try { return sessionStorage.getItem(tokenKey) || ''; }
  catch { return ''; }
}

function groupChats(chats: Chat[]): { label: string; chats: Chat[] }[] {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const yesterday = new Date(today);
  yesterday.setDate(yesterday.getDate() - 1);
  const week = new Date(today);
  week.setDate(week.getDate() - 7);
  const groups = ['今天', '昨天', '最近 7 天', '更早'].map(label => ({ label, chats: [] as Chat[] }));
  for (const chat of chats) {
    const updated = Date.parse(chat.updatedAt);
    const index = updated >= today.getTime() ? 0 : updated >= yesterday.getTime() ? 1 : updated >= week.getTime() ? 2 : 3;
    groups[index].chats.push(chat);
  }
  return groups.filter(group => group.chats.length > 0);
}

export default function App() {
  const [token, setToken] = useState(readToken);
  const [authRevision, setAuthRevision] = useState(0);
  return <Workspace key={authRevision} token={token} onTokenChange={(next) => {
    if (next !== token) {
      setToken(next);
      setAuthRevision((revision) => revision + 1);
    }
  }} />;
}

function Workspace({ token, onTokenChange }: { token: string; onTokenChange: (token: string) => void }) {
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [instancesOpen, setInstancesOpen] = useState(() => window.location.hash === '#instances');
  const [draft, setDraft] = useState('');
  const [storageError, setStorageError] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const textarea = useRef<HTMLTextAreaElement>(null);
  const sidebar = useRef<HTMLElement>(null);
  const sidebarTrigger = useRef<HTMLButtonElement>(null);
  const chat = useChat(token);
  const running = ['streaming', 'cancelling', 'blocked'].includes(chat.phase);
  const busy = chat.phase !== 'idle' || submitting;
  const navigationDisabled = ['cancelling', 'syncing'].includes(chat.phase) || submitting;

  useEffect(() => {
    const input = textarea.current;
    if (input) {
      input.style.height = 'auto';
      input.style.height = `${Math.min(input.scrollHeight, 180)}px`;
    }
  }, [draft]);

  useEffect(() => {
    if (!sidebarOpen || !window.matchMedia('(max-width: 760px)').matches) return;
    const panel = sidebar.current;
    const focusable = () => Array.from(panel?.querySelectorAll<HTMLButtonElement>('button:not(:disabled)') || []);
    focusable()[0]?.focus();
    const handleKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setSidebarOpen(false);
      if (event.key !== 'Tab') return;
      const buttons = focusable();
      const first = buttons[0];
      const last = buttons[buttons.length - 1];
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    };
    document.addEventListener('keydown', handleKey);
    return () => { document.removeEventListener('keydown', handleKey); sidebarTrigger.current?.focus(); };
  }, [sidebarOpen]);

  const showInstances = (open: boolean) => {
    setInstancesOpen(open);
    const base = `${window.location.pathname}${window.location.search}`;
    window.history.replaceState(null, '', open ? `${base}#instances` : base);
  };

  const navigate = async (target: Chat | null) => {
    setSidebarOpen(false);
    setDraft('');
    showInstances(false);
    await chat.selectChat(target);
  };

  const submit = async () => {
    if (busy || !draft.trim() || !token || !chat.historyReady) return;
    setSubmitting(true);
    try {
      if (await chat.send(draft)) setDraft('');
    } finally { setSubmitting(false); }
  };

  const saveToken = (next: string) => {
    if (/[\u0000-\u0020\u007f]/.test(next)) {
      setStorageError('访问令牌不能包含空格、换行或控制字符。');
      return;
    }
    try {
      if (next) sessionStorage.setItem(tokenKey, next);
      else sessionStorage.removeItem(tokenKey);
      onTokenChange(next);
      setSettingsOpen(false);
      setStorageError('');
      setDraft('');
    } catch { setStorageError('浏览器禁止会话存储，无法保存令牌。请允许此站点使用 sessionStorage。'); }
  };

  return (
    <div className="app-shell">
      {sidebarOpen && <div className="sidebar-backdrop" onClick={() => setSidebarOpen(false)} />}
      <aside ref={sidebar} className={`sidebar ${sidebarOpen ? 'is-open' : ''}`} aria-label="会话导航">
        <div className="sidebar-brand"><span className="brand-mark"><Sparkles size={21} /></span><span>peri<span className="brand-dot">.</span></span>
          <button type="button" className="icon-button mobile-only" onClick={() => setSidebarOpen(false)} aria-label="关闭会话列表"><PanelLeftClose size={19} /></button>
        </div>
        <button className="new-chat-button" type="button" disabled={navigationDisabled} onClick={() => void navigate(null)}><Plus size={18} />开启新对话<span>↗</span></button>
        <div className="sidebar-section-heading"><span>你的对话</span>
          <button className="icon-button" type="button" aria-label="刷新会话列表" disabled={!token || chat.listLoading}
            onClick={() => void chat.refreshList()}><RefreshCw size={14} className={chat.listLoading ? 'spinning' : ''} /></button>
        </div>
        <nav className="chat-list" aria-label="历史会话" aria-busy={chat.listLoading}>
          {chat.listLoading && chat.chats.length === 0 ? <div className="list-placeholder">正在加载对话…</div>
            : chat.chats.length === 0 ? <div className="sidebar-empty"><MessageSquare size={24} /><p>{token ? '还没有对话' : '你的想法，值得被记录'}</p><span>{token ? '从一个问题开始吧' : '连接后，在这里找回每次思考'}</span></div>
              : groupChats(chat.chats).map(group => <section className="chat-group" key={group.label}><h2>{group.label}</h2>
                {group.chats.map(item => <button type="button" className={`chat-item ${chat.selected?.id === item.id ? 'selected' : ''}`}
                  key={item.id} data-chat-id={item.id} title={item.title || '新对话'} aria-current={chat.selected?.id === item.id ? 'page' : undefined}
                  disabled={navigationDisabled} onClick={() => void navigate(item)}><MessageSquare size={15} /><span>{item.title || '新对话'}</span></button>)}
              </section>)}
        </nav>
        <div className="sidebar-bottom"><div className="workspace-note"><span className={`connection-dot ${token ? 'connected' : ''}`} />{token ? '已配置访问令牌' : '尚未连接'}<ShieldCheck size={14} /></div>
          <button type="button" className="settings-button" disabled={busy} onClick={() => setSettingsOpen(true)}><span className="user-avatar">你</span><span>我的空间<small>设置与访问令牌</small></span><Settings2 size={18} /></button>
        </div>
      </aside>
      {instancesOpen && <Suspense fallback={<div className="main-panel instances-loading" role="status">
        <RefreshCw className="spinning" size={22} /><p>正在加载实例监控…</p></div>}>
        <InstancesPanel token={token} onClose={() => showInstances(false)} onOpenChat={(chatId) => {
          const target = chat.chats.find((item) => item.id === chatId);
          if (target) void navigate(target);
          else showInstances(false);
        }} />
      </Suspense>}
      {!instancesOpen && <main className="main-panel">
        <header className="topbar"><div className="topbar-left"><button ref={sidebarTrigger} type="button" className="icon-button mobile-only" aria-label="打开会话列表" aria-expanded={sidebarOpen} onClick={() => setSidebarOpen(true)}><Menu size={21} /></button>
          <span className="topbar-name">Peri <ChevronDown size={14} /></span><span className="topbar-divider" /><span className="topbar-subtitle">{chat.selected?.title || '让想法，自由生长'}</span></div>
          <div className="topbar-actions">
            <button type="button" className="icon-button" aria-label="实例监控" title="实例监控" disabled={!token}
              onClick={() => showInstances(true)}><Activity size={18} /></button>
            <span className="topbar-badge"><span />专注于你的下一步</span>
          </div>
        </header>
        <div className="content-area">
          {chat.selected && !chat.historyReady && chat.phase !== 'loading' && !chat.error && <div className="error-banner" role="status"><span>正在同步会话，连接就绪后可以发送消息。</span></div>}
          {(chat.error || storageError) && <div className="error-banner" role="alert"><span>{storageError || chat.error}</span>
            {!chat.historyReady && !running && <button type="button" className="text-button" disabled={chat.phase === 'loading'} onClick={() => void chat.selectChat(chat.selected)}>重新加载</button>}
            <button type="button" className="icon-button" aria-label="关闭错误提示" onClick={() => { setStorageError(''); chat.dismissError(); }}><X size={17} /></button>
          </div>}
          {chat.phase === 'loading' && !submitting ? <div className="loading-state" role="status"><RefreshCw className="spinning" size={22} /><p>正在加载会话…</p></div>
            : chat.messages.length > 0 ? <Suspense fallback={<div role="status">正在加载消息…</div>}>
              <MessageList messages={chat.messages} phase={chat.phase} chatId={chat.selected?.id} />
            </Suspense>
              : <div className="welcome"><div className="welcome-symbol"><Sparkles size={33} strokeWidth={1.35} /></div>
                <div className="welcome-eyebrow">一点好奇，无限可能</div><h1>今天，我们一起<br className="mobile-only" />探索什么？</h1><p className="welcome-description">一个问题、一闪灵感，或一件想完成的事。<br />从这里开始，把想法变成下一步。</p>
                <div className="suggestions">{suggestions.map(item => <button type="button" className="suggestion-card" key={item.title}
                  disabled={busy || !chat.historyReady} onClick={() => { setDraft(item.prompt); textarea.current?.focus(); }}>
                  <span className={`suggestion-icon ${item.color}`}><item.icon size={19} strokeWidth={1.6} /></span><ArrowRight className="suggestion-arrow" size={16} />
                  <strong>{item.title}</strong><span className="suggestion-description">{item.description}</span></button>)}</div>
                {!token && <button className="connect-prompt" type="button" onClick={() => setSettingsOpen(true)}><ShieldCheck size={15} />设置访问令牌，开始对话<ArrowRight size={14} /></button>}
              </div>}
          <div className="composer-area"><form className={`composer ${running ? 'is-generating' : ''}`} onSubmit={event => { event.preventDefault(); void submit(); }}>
            <textarea ref={textarea} aria-label="输入消息" placeholder={token ? '有任何想法，都可以从这里开始…' : '先设置访问令牌，再开始对话…'}
              rows={1} value={draft} disabled={!token || busy || !chat.historyReady} onChange={event => setDraft(event.target.value)}
              onKeyDown={event => { if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing && event.keyCode !== 229) { event.preventDefault(); void submit(); } }} />
            <div className="composer-toolbar"><span className="composer-hint"><Sparkles size={14} />{running ? '思考正在发生' : '和 Peri 一起思考'}</span><div className="composer-actions"><span className="keyboard-hint">Enter 发送 · Shift + Enter 换行</span>
              {running ? <button className="send-button stop-button" type="button" disabled={chat.phase === 'cancelling'} aria-label={chat.phase === 'cancelling' ? '等待服务端停止' : '停止生成'} title="停止生成" onClick={() => void chat.stop()}><Square size={15} fill="currentColor" /></button>
                : <button className="send-button" type="submit" aria-label="发送消息" disabled={!token || busy || !draft.trim() || !chat.historyReady}><ArrowUp size={20} /></button>}</div></div>
          </form><p className="composer-disclaimer">AI 也会有不确定的时候，重要信息请记得核实。<span>保持好奇，也保持判断。</span></p></div>
        </div>
      </main>}
      {settingsOpen && <Settings token={token} error={storageError} onSave={saveToken} onClose={() => { setSettingsOpen(false); setStorageError(''); }} />}
    </div>
  );
}
