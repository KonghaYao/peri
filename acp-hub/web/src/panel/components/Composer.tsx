// 发送窗口（Composer）：输入区 + 底部工具行（ui.md §3.8 / §四.7）。
//
// 由 ChatView 拆出（右区三区之一）：悬浮大圆角卡片，textarea 自动增高
// （max 180px 后内部滚动），Enter 发送 / Shift+Enter 换行（含 IME
// 组合态防护）；底部工具行显示模型 / effort / 上下文占用（均来自 agent
// map，server 写入的真实配置）收进一个安静的运行标识，完整值
// 通过 title 可发现；发送 / 停止主动作始终保留。
// 对话操作（新建/新会话/取消/关闭）已收敛到左侧对话列表区。

import { Show } from 'solid-js';
import { cancelTurn, cancellingTurn, chatHead, chatStatusSignal, clearComposerDraft, composerDraft, dismissMessageSubmission, isTerminal, messageSubmission, navigateProjectSession, openingSessionId, projectSessions, readOnly, retryMessageSubmission, runtimeDocsHydrated, selectedCid, selectedSessionId, sendMessage, setComposerDraft, turnActive } from '../store';
import { Button, CopyButton, Icon, IconButton, Textarea } from '../../ui';

/** tokens 数值 → "12k"/"200k" 缩写（>=1000 取 k；非法值 → null）。 */
function fmtTokens(n: number | null): string | null {
  if (n === null) return null;
  if (n >= 1000) return `${Math.round(n / 1000)}k`;
  return String(n);
}

export function Composer() {
  let taRef: HTMLTextAreaElement | undefined;
  const submissionForSession = () => messageSubmission()?.sessionId === selectedSessionId() ? messageSubmission() : null;
  const submissionInAnotherSession = () => messageSubmission() && !submissionForSession() ? messageSubmission() : null;
  const pendingSessionTitle = () => {
    const submission = submissionInAnotherSession();
    return projectSessions().find((session) => session.id === submission?.sessionId)?.title || '另一会话';
  };

  const terminal = () => isTerminal(chatStatusSignal()[selectedCid() ?? '']);
  const inputDisabled = () => !selectedCid() || !runtimeDocsHydrated() || terminal() || !!openingSessionId() || readOnly() || turnActive() || !!messageSubmission();
  const inputPlaceholder = () =>
    readOnly() ? '只读模式' : openingSessionId() ? '正在打开会话…' : !selectedCid()
      ? '先从左侧选择或新建会话'
      : !runtimeDocsHydrated() ? '正在载入会话…'
      : terminal()
        ? '对话已结束（历史只读）'
        : turnActive() ? 'Agent 正在工作，可随时停止' : submissionForSession() ? '正在确认当前消息…' : submissionInAnotherSession() ? '另一会话的消息仍在确认…' : '给 Agent 发消息';

  // 信息行三个真实值（agent map，server 写入；缺失 → —）。
  const model = () => chatHead()?.agent?.model || '—';
  const effort = () => chatHead()?.agent?.effort || '—';

  // 上下文占用（tokens）：12k/200k；任一缺失显示 —。
  const ctxText = () => {
    const used = fmtTokens(chatHead()?.agent?.contextUsed ?? null);
    const cap = fmtTokens(chatHead()?.agent?.contextWindow ?? null);
    if (used === null || cap === null) return '—';
    return `${used}/${cap}`;
  };
  const runtimeSummary = () => {
    const parts = [`模型 ${model()}`];
    if (effort() !== '—') parts.push(`推理强度 ${effort()}`);
    if (ctxText() !== '—') parts.push(`上下文 ${ctxText()}`);
    return parts.join(' · ');
  };

  function submit() {
    const text = composerDraft().trim();
    if (!text) return;
    if (!sendMessage(text)) return;
    if (taRef) {
      // 先同步清空 DOM 值再测量：value 绑定是延迟 effect，若在
      // setMsg('') 后立即测 scrollHeight 会测到旧多行文本的高度，
      // 发送后 Composer 保持展开高度不收回（min-h 52px 兜底）
      taRef.value = '';
      taRef.style.height = 'auto';
      taRef.style.height = `${taRef.scrollHeight}px`;
    }
    const sessionId = selectedSessionId();
    if (sessionId) clearComposerDraft(sessionId);
  }

  return (
    <div class="composer-wrap">
      <section
        aria-disabled={inputDisabled()}
        class="composer-surface"
      >
        <Textarea
          ref={taRef}
          autoResize
          maxHeight={180}
          value={composerDraft()}
          onInput={(e) => {
            const el = e.currentTarget;
            setComposerDraft(el.value);
          }}
          onKeyDown={(e) => {
            if (e.isComposing) return; // IME 组合确认回车不误发
            if (e.key === 'Enter' && !e.shiftKey) {
              e.preventDefault();
              submit();
            }
          }}
          placeholder={inputPlaceholder()}
          disabled={inputDisabled()}
          spellcheck={false}
          class="composer-input ui-scrollbar"
        />
        <div class="composer-toolbar">
          <span class="composer-runtime" title={runtimeSummary()} aria-label={runtimeSummary()}>
            <span aria-hidden="true" />{model()}
          </span>
          <span class="composer-shortcut" aria-hidden="true">Enter 发送 · Shift + Enter 换行</span>
          <Show when={turnActive()} fallback={
            <IconButton tooltipPlacement="end" variant="primary" type="button" onClick={submit} disabled={inputDisabled() || !composerDraft().trim()} label="发送" class="composer-action">
              <Icon><path d="M10 16V4" /><path d="M5 9l5-5 5 5" /></Icon>
            </IconButton>
          }>
            <IconButton tooltipPlacement="end" variant="primary" type="button" onClick={cancelTurn} disabled={cancellingTurn() || readOnly()} busy={cancellingTurn()} label="停止生成" class="composer-action composer-action--stop">
              <Show when={!cancellingTurn()}><span aria-hidden="true" /></Show>
            </IconButton>
          </Show>
        </div>
      </section>
      <Show when={submissionForSession()}>{(submission) =>
        <section class={`submission-state submission-state--${submission().phase}`} role={submission().phase === 'failed' || submission().phase === 'uncertain' ? 'alert' : 'status'}>
          <div class="submission-state__body"><strong>{submission().phase === 'uncertain' ? '结果尚未确认' : submission().phase === 'failed' ? '消息未发送' : '正在提交消息'}</strong><p>{submission().detail || '在服务器确认前，我们会保留这条消息。'}</p><details><summary>查看原消息</summary><pre>{submission().text}</pre></details></div>
          <Show when={submission().phase === 'failed' || submission().phase === 'uncertain'}>
            <div class="submission-state__actions"><CopyButton text={submission().text} label="复制原文" size="compact" /><Show when={submission().retryable}><Button variant="primary" size="compact" onClick={retryMessageSubmission}>使用同一请求重新确认</Button></Show><Show when={submission().phase === 'failed'}><Button size="compact" onClick={dismissMessageSubmission}>返回编辑</Button></Show></div>
          </Show>
        </section>
      }</Show>
      <Show when={submissionInAnotherSession()}>{(submission) =>
        <section class="submission-state submission-state--foreign" role="status">
          <div class="submission-state__body"><strong>另一会话仍在确认</strong><p>“{pendingSessionTitle()}”还有一条消息等待服务器终态。为避免重复执行，确认完成前暂不发送新消息。</p></div>
          <div class="submission-state__actions"><Button size="compact" onClick={() => navigateProjectSession(submission().sessionId)}>返回该会话</Button></div>
        </section>
      }</Show>
    </div>
  );
}
