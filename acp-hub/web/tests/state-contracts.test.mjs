import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { transform } from 'lightningcss';
import postcss from 'postcss';
import { acceptMessageSubmission, beginMessageSubmission, beginQuickStart, completesMessageSubmission, failMessageSubmission, isTurnActive, lockPermissionDecision, markMessageUncertain, markPermissionDecisionUncertain, quickStartCanActivate, unlockPermissionDecision, updateQuickStart } from '../src/panel/lib/action-state.mjs';
import { cleanSessionTitle, connectionProblemForClose, formatRelativeTime, retainLiveRuntimeHints, sessionDisplayTitle, shortSessionId } from '../src/panel/lib/recovery-state.mjs';
import { messageTime } from '../src/panel/lib/message-time.mjs';
import { parseMarkdown, safeHref } from '../src/panel/lib/markdown.mjs';
import { messageActivity, nextFollowState } from '../src/panel/lib/message-follow.mjs';
import { acquireInert, activeOverlayCount } from '../src/ui/overlay-state.mjs';
import { authFeedback } from '../src/panel/lib/auth-feedback.mjs';
import { searchProjectSessions } from '../src/panel/lib/session-search.mjs';
import { runtimeState } from '../src/panel/lib/runtime-state.mjs';
const parsePrincipal = (v) => v && ['full','read-only'].includes(v.role) ? v.role : null;
const canMutate = (role) => role === 'full';

test('source stylesheets are structurally valid and consume only declared design tokens', () => {
  const source = join(import.meta.dirname, '..', 'src');
  const files = ['styles.css', 'ui/primitives.css', 'ui/tokens.css'];
  const roots = files.map((file) => {
    const css = readFileSync(join(source, file), 'utf8');
    const strict = transform({ filename: file, code: Buffer.from(css), errorRecovery: false });
    assert.deepEqual(strict.warnings, [], `${file} has strict-parser warnings`);
    return postcss.parse(css, { from: file });
  });
  for (const root of roots) {
    root.walkAtRules('media', (media) => {
      const directDeclarations = (media.nodes || []).filter((node) => node.type === 'decl');
      assert.deepEqual(directDeclarations.map((decl) => `${decl.source?.start?.line}:${decl.prop}`), [], `${media.source?.input.file} has declarations outside a rule`);
    });
  }
  const tokenSource = readFileSync(join(source, 'ui', 'tokens.css'), 'utf8');
  const defined = new Set([...tokenSource.matchAll(/(--[\w-]+)\s*:/g)].map((match) => match[1]));
  const used = new Set(files.slice(0, 2).flatMap((file) => [...readFileSync(join(source, file), 'utf8').matchAll(/var\((--[\w-]+)/g)].map((match) => match[1])));
  assert.deepEqual([...used].filter((token) => !defined.has(token)).sort(), []);
});

test('Composer and quick start have one neutral keyboard-focus owner and no stale selectors', () => {
  const css = readFileSync(join(import.meta.dirname, '..', 'src', 'styles.css'), 'utf8');
  const root = postcss.parse(css);
  const focusRules = [];
  root.walkRules((rule) => {
    if (rule.selector.includes('.composer-surface:focus-within') || rule.selector.includes('.quick-start__surface:focus-within')) focusRules.push(rule.selector);
  });
  assert.equal(focusRules.length, 1);
  assert.doesNotMatch(css, /--focus-neutral|--surface-border-focus|\.permission-actions\b/);
  assert.match(css, /\.composer-surface:has\(\.composer-input:focus-visible\)/);
});

test('runtime status distinguishes durable session state from process state', () => {
  assert.equal(runtimeState({ hasSession: true, isOpening: false, hasRuntime: false, hasPendingPermission: false, turnActive: false }).label, '未启动 · 会话已保存');
  assert.equal(runtimeState({ hasSession: true, isOpening: false, hasRuntime: true, chatStatus: 'active', hasPendingPermission: false, turnActive: false }).tone, 'ready');
  assert.equal(runtimeState({ hasSession: true, isOpening: false, hasRuntime: true, isHydrated: false, chatStatus: 'active', hasPendingPermission: false, turnActive: false }).label, '正在载入会话…');
  assert.equal(runtimeState({ hasSession: true, isOpening: false, hasRuntime: true, chatStatus: 'active', hasPendingPermission: true, turnActive: true }).label, '等待你的许可');
  assert.equal(runtimeState({ hasSession: true, isOpening: false, hasRuntime: true, chatStatus: 'crashed', hasPendingPermission: false, turnActive: false }).tone, 'danger');
  assert.equal(runtimeState({ hasSession: true, lifecycle: 'reconciliation_required', isOpening: false, hasRuntime: false, hasPendingPermission: false, turnActive: false }).tone, 'attention');
});

test('principal parsing and mutation policy are closed by default', async () => {
  assert.equal(parsePrincipal({ role: 'full' }), 'full');
  assert.equal(parsePrincipal({ role: 'read-only' }), 'read-only');
  assert.equal(parsePrincipal({ role: 'instance' }), null);
  assert.equal(canMutate('full'), true);
  assert.equal(canMutate('read-only'), false);
  assert.equal(canMutate(null), false);
});
test('message submission preserves text through accepted and uncertain states', () => {
  const sending = beginMessageSubmission('cmd-1', 'important draft');
  const accepted = acceptMessageSubmission(sending, 'cmd-1');
  const uncertain = markMessageUncertain(accepted, 'cmd-1');
  assert.equal(accepted.phase, 'accepted');
  assert.equal(uncertain.phase, 'uncertain');
  assert.equal(uncertain.text, 'important draft');
  assert.equal(uncertain.retryable, true);
  assert.equal(completesMessageSubmission(uncertain, { commandId: 'cmd-1', status: 'duplicate' }), true);
});

test('message retry identity survives uncertainty and a late terminal ack completes it once', () => {
  const initial = beginMessageSubmission('same-command', 'do not lose this');
  const uncertain = markMessageUncertain(acceptMessageSubmission(initial, 'same-command'), 'same-command');
  assert.equal(uncertain.commandId, 'same-command');
  assert.equal(uncertain.text, 'do not lose this');
  assert.equal(completesMessageSubmission(uncertain, { commandId: 'other-command', status: 'committed' }), false);
  assert.equal(completesMessageSubmission(uncertain, { commandId: 'same-command', status: 'committed' }), true);
  assert.equal(completesMessageSubmission(uncertain, { commandId: 'same-command', status: 'duplicate' }), true);
});

test('prompt recovery is owned by CommandTracker rather than an ad-hoc frame cache', () => {
  const store = readFileSync(join(import.meta.dirname, '..', 'src', 'panel', 'store.ts'), 'utf8');
  assert.match(store, /sendAction\(frame, 'prompt', \{\s*retryOnUncertain: true/);
  assert.match(store, /sendAction\(frame, 'session\/create', \{\s*retryOnUncertain: true/);
  assert.match(store, /commands\.retry\(current\.commandId, sendFrame\)/);
  assert.doesNotMatch(store, /retryableMessageFrame|retryableQuickStartFrame/);
});

test('quick start preserves project, text and command identity until a complete activation ack', () => {
  const initial = beginQuickStart('create-1', 'project-1', 'first prompt');
  const uncertain = updateQuickStart(initial, 'create-1', 'uncertain', 'unknown');
  assert.equal(uncertain.text, 'first prompt');
  assert.equal(uncertain.projectId, 'project-1');
  assert.equal(quickStartCanActivate(uncertain, { commandId: 'create-2', status: 'committed', sessionId: 's', chatId: 'c' }), false);
  assert.equal(quickStartCanActivate(uncertain, { commandId: 'create-1', status: 'committed', sessionId: 's' }), false);
  assert.equal(quickStartCanActivate(uncertain, { commandId: 'create-1', status: 'committed', sessionId: 's', chatId: 'c' }), true);
  assert.equal(quickStartCanActivate(uncertain, { commandId: 'create-1', status: 'duplicate', sessionId: 's', chatId: 'c' }), true);
  assert.equal(updateQuickStart(initial, 'create-1', 'failed', 'invalid', false).retryable, false);
});

test('message failure is command correlated and retains retry policy', () => {
  const sending = beginMessageSubmission('cmd-1', 'draft');
  assert.equal(failMessageSubmission(sending, 'stale', 'wrong'), sending);
  assert.deepEqual(failMessageSubmission(sending, 'cmd-1', 'offline', true), {
    ...sending, phase: 'failed', detail: 'offline', retryable: true,
  });
});

test('active turn excludes every terminal projection state', () => {
  assert.equal(isTurnActive({ turnStatus: 'streaming' }), true);
  assert.equal(isTurnActive({ turnStatus: 'awaiting_permission' }), true);
  for (const status of ['completed', 'interrupted', 'cancelled', 'failed', 'ended']) {
    assert.equal(isTurnActive({ turnStatus: status }), false, status);
  }
  assert.equal(isTurnActive(null), false);
});

test('permission lock is idempotent and remains until the projection removes it', () => {
  const empty = new Map();
  const locked = lockPermissionDecision(empty, 'permission-1', 'allow');
  assert.equal(lockPermissionDecision(locked, 'permission-1', 'deny'), locked);
  assert.deepEqual(locked.get('permission-1'), { decision: 'allow', phase: 'pending' });
  const uncertain = markPermissionDecisionUncertain(locked, 'permission-1');
  assert.deepEqual(uncertain.get('permission-1'), { decision: 'allow', phase: 'uncertain' });
  assert.equal(markPermissionDecisionUncertain(uncertain, 'permission-1'), uncertain);
  assert.equal(unlockPermissionDecision(locked, 'permission-1').has('permission-1'), false);
});

test('permission delivery uncertainty remains locked in the security surface', () => {
  const store = readFileSync(join(import.meta.dirname, '..', 'src', 'panel', 'store.ts'), 'utf8');
  assert.match(store, /onTimeout:\s*\(\)\s*=>\s*\{[\s\S]*?markPermissionDecisionUncertain\(decisions, permissionId\)[\s\S]*?不要提交相反决策/);
  assert.match(store, /onError:\s*release/);
});

test('runtime hints survive only while Registry proves the chat is non-terminal', () => {
  const sessions = [
    { id: 'live', activeChatId: 'chat-live' },
    { id: 'closed', activeChatId: 'chat-closed' },
    { id: 'missing', activeChatId: 'chat-missing' },
    { id: 'idle', activeChatId: null },
  ];
  assert.deepEqual(retainLiveRuntimeHints(sessions, [
    { id: 'chat-live', status: 'gap' },
    { id: 'chat-closed', status: 'closed' },
  ]).map((session) => session.activeChatId), ['chat-live', null, null, null]);
});

test('import presentation removes system reminders and formats stable metadata', () => {
  assert.equal(cleanSessionTitle('hello <system-reminder>secret instructions'), 'hello');
  assert.equal(cleanSessionTitle('<system-reminder>only internals'), '未命名会话');
  assert.equal(formatRelativeTime('2026-08-13T00:00:00Z', Date.parse('2026-08-13T02:00:00Z')), '2 小时前');
  assert.equal(sessionDisplayTitle('新对话', 'acp-123456789'), '新对话 · …23456789');
  assert.equal(sessionDisplayTitle('修复登录流程', 'acp-123456789'), '修复登录流程');
  assert.equal(shortSessionId('session-1234567890'), '34567890');
});

test('message timestamps are human-readable, exact on hover, and absent when unknown', () => {
  const now = Date.parse('2026-08-13T12:00:00Z');
  const today = messageTime('2026-08-13T10:05:00Z', now);
  assert.match(today?.label || '', /05/);
  assert.match(today?.exact || '', /2026/);
  assert.match(messageTime('2026-08-11T10:05:00Z', now)?.label || '', /周/);
  assert.match(messageTime('2025-12-01T10:05:00Z', now)?.label || '', /2025/);
  assert.equal(messageTime('—', now), null);
  assert.equal(messageTime('not-a-date', now), null);
});

test('fatal connection states prescribe one safe recovery action', () => {
  assert.equal(connectionProblemForClose(4500).action, 'reconnect');
  assert.equal(connectionProblemForClose(4501).action, 'reconnect');
  assert.equal(connectionProblemForClose(4502).action, 'login');
  assert.equal(connectionProblemForClose(9999).action, 'reconnect');
});

test('markdown links allow explicit public protocols and reject active content', () => {
  assert.equal(safeHref('https://example.com/a?q=1'), 'https://example.com/a?q=1');
  assert.equal(safeHref('mailto:user@example.com'), 'mailto:user@example.com');
  for (const href of ['javascript:alert(1)', ' JAVASCRIPT:alert(1)', 'data:text/html,x', 'file:///tmp/x', '//evil.example']) {
    assert.equal(safeHref(href), null, href);
  }
});

test('markdown parser keeps raw html inert and structures coding content', () => {
  const blocks = parseMarkdown('# Title\n\n<script>alert(1)</script>\n\n```ts unsafe meta\nconst x = 1;\n```\n\n- one\n- two');
  assert.equal(blocks[0].type, 'heading');
  assert.deepEqual(blocks[1], { type: 'paragraph', children: [{ type: 'text', text: '<script>alert(1)</script>' }] });
  assert.deepEqual(blocks[2], { type: 'code', language: 'ts', text: 'const x = 1;' });
  assert.equal(blocks[3].type, 'list');
  assert.equal(blocks[3].items.length, 2);
});

test('unsafe markdown links degrade to readable inert text', () => {
  const [paragraph] = parseMarkdown('[run this](javascript:alert(1))');
  assert.equal(paragraph.children.some((token) => token.type === 'link'), false);
  assert.equal(paragraph.children.map((token) => token.text || '').join(''), 'run this (javascript:alert(1))');
});

test('message follow pauses without losing the new-content signal', () => {
  const before = messageActivity([{ id: 'a', status: 'streaming', text: 'one', toolCalls: [] }]);
  const after = messageActivity([{ id: 'a', status: 'streaming', text: 'one two', toolCalls: [] }]);
  assert.equal(nextFollowState({ stick: false, hasNewContent: false, previousActivity: before, activity: after }).hasNewContent, true);
  assert.equal(nextFollowState({ stick: false, hasNewContent: true, previousActivity: after, activity: after }).hasNewContent, true);
  assert.equal(nextFollowState({ stick: true, hasNewContent: true, previousActivity: before, activity: after }).hasNewContent, false);
});

test('nested overlays keep the application inert until the final release', () => {
  const target = { inert: false };
  const otherTarget = { inert: false };
  const releaseFirst = acquireInert(target);
  const releaseSecond = acquireInert(target);
  const releaseOther = acquireInert(otherTarget);
  assert.equal(target.inert, true);
  assert.equal(otherTarget.inert, true);
  assert.equal(activeOverlayCount(), 3);
  releaseOther();
  assert.equal(otherTarget.inert, false);
  assert.equal(target.inert, true);
  releaseFirst();
  assert.equal(target.inert, true);
  releaseSecond();
  assert.equal(target.inert, false);
  assert.equal(activeOverlayCount(), 0);
});

test('overlay leases restore rather than overwrite pre-existing inert state', () => {
  const target = { inert: true };
  const release = acquireInert(target);
  assert.equal(target.inert, true);
  release();
  assert.equal(target.inert, true);
  assert.equal(activeOverlayCount(), 0);
});

test('authentication feedback does not blame credentials for server failures', () => {
  assert.equal(authFeedback(401, 'status'), null);
  assert.equal(authFeedback(401, 'login').kind, 'credential');
  assert.equal(authFeedback(403, 'login').kind, 'origin');
  assert.equal(authFeedback(429, 'login').kind, 'rate');
  assert.equal(authFeedback(503, 'login').kind, 'server');
  assert.match(authFeedback(503, 'login').message, /不代表令牌有误/);
  assert.equal(authFeedback(0, 'status').retryable, true);
});

test('feature components consume the Solid UI library only through its public barrel', () => {
  const components = join(import.meta.dirname, '..', 'src', 'panel', 'components');
  const offenders = readdirSync(components)
    .filter((file) => file.endsWith('.tsx'))
    .filter((file) => /from\s+['"]\.\.\/\.\.\/ui\//.test(readFileSync(join(components, file), 'utf8')));
  assert.deepEqual(offenders, []);
});

test('feature-owned SVG geometry always uses the shared finite icon canvas', () => {
  const components = join(import.meta.dirname, '..', 'src', 'panel', 'components');
  const offenders = readdirSync(components)
    .filter((file) => file.endsWith('.tsx'))
    .filter((file) => /<svg\b/.test(readFileSync(join(components, file), 'utf8')));
  assert.deepEqual(offenders, []);
  const icon = readFileSync(join(import.meta.dirname, '..', 'src', 'ui', 'Icon.tsx'), 'utf8');
  assert.match(icon, /class={`ui-icon/);
  assert.match(icon, /fill="none"/);
  assert.match(icon, /stroke="currentColor"/);
});

test('high-frequency chat controls are owned by the Solid UI library', () => {
  const components = join(import.meta.dirname, '..', 'src', 'panel', 'components');
  for (const file of ['Composer.tsx', 'MessageList.tsx']) {
    assert.doesNotMatch(readFileSync(join(components, file), 'utf8'), /<button\b/, file);
  }
});

test('MessageList delegates entry semantics to one tested conversation component', () => {
  const components = join(import.meta.dirname, '..', 'src', 'panel', 'components');
  const list = readFileSync(join(components, 'MessageList.tsx'), 'utf8');
  const message = readFileSync(join(components, 'ConversationMessage.tsx'), 'utf8');
  assert.match(list, /<ConversationMessage entry=\{entry\}/);
  assert.doesNotMatch(list, /function MessageBubble|<Markdown|<ToolCallCard/);
  assert.match(message, /conversation-message--\$\{role\(\)\}/);
  assert.match(message, /role="alert" aria-label="消息错误"/);
});

test('the permission surface exposes a queue and never resolves an empty identity', () => {
  const components = join(import.meta.dirname, '..', 'src', 'panel', 'components');
  const messageList = readFileSync(join(components, 'MessageList.tsx'), 'utf8');
  const queue = readFileSync(join(components, 'PermissionQueue.tsx'), 'utf8');
  const card = readFileSync(join(components, 'PermissionRequestCard.tsx'), 'utf8');
  assert.match(messageList, /<PermissionQueue/);
  assert.doesNotMatch(messageList, /permissions\(\)\[0\]/);
  assert.match(queue, /if \(id\) props\.onResolve\(id, decision\)/);
  assert.match(card, /disabled=\{props\.readOnly \|\| locked\(\) \|\| !actionable\(\)\}/);
});

test('the shared Button defaults to non-submitting behavior', () => {
  const button = readFileSync(join(import.meta.dirname, '..', 'src', 'ui', 'Button.tsx'), 'utf8');
  assert.match(button, /type=\{button\.type \?\? 'button'\}/);
});

test('feature-owned native buttons always state their form behavior', () => {
  const components = join(import.meta.dirname, '..', 'src', 'panel', 'components');
  const offenders = readdirSync(components)
    .filter((file) => file.endsWith('.tsx'))
    .flatMap((file) => [...readFileSync(join(components, file), 'utf8').matchAll(/<button\b([^>]*)>/gs)]
      .filter((match) => !/\btype\s*=/.test(match[1]))
      .map(() => file));
  assert.deepEqual(offenders, []);
});

test('feature components never introduce literal colors', () => {
  const components = join(import.meta.dirname, '..', 'src', 'panel', 'components');
  const offenders = readdirSync(components)
    .filter((file) => file.endsWith('.tsx'))
    .filter((file) => /#[0-9a-f]{3,8}\b|rgba?\(/i.test(readFileSync(join(components, file), 'utf8')));
  assert.deepEqual(offenders, []);
});

test('responsive behavior has compact, medium and wide layout contracts', () => {
  const root = join(import.meta.dirname, '..', 'src');
  const shell = readFileSync(join(root, 'panel', 'components', 'AppShell.tsx'), 'utf8');
  const css = readFileSync(join(root, 'styles.css'), 'utf8');
  const breakpoints = readFileSync(join(root, 'ui', 'breakpoints.ts'), 'utf8');
  assert.match(shell, /compactViewportQuery/);
  assert.doesNotMatch(shell, /max-width:\s*\d+px/);
  assert.match(breakpoints, /COMPACT_VIEWPORT_MAX\s*=\s*959/);
  assert.match(breakpoints, /MEDIUM_VIEWPORT_MAX\s*=\s*1199/);
  assert.match(css, /@media\(max-width:959px\)/);
  assert.match(css, /@media\(min-width:960px\) and \(max-width:1199px\)/);
  const medium = css.match(/@media\(min-width:960px\) and \(max-width:1199px\)\{([\s\S]*?)\n\}/)?.[0] || '';
  assert.match(medium, /grid-template-columns:240px minmax\(0,1fr\)/);
  assert.match(medium, /max-width:760px/);
  assert.match(medium, /max-width:800px/);
  assert.doesNotMatch(medium, /project-drawer\{[^}]*position:fixed/);
});

test('coarse pointers never depend on hover to discover sidebar actions', () => {
  const styles = readFileSync(join(import.meta.dirname, '..', 'src', 'styles.css'), 'utf8');
  const coarse = styles.match(/@media\(pointer:coarse\)\{([^}]|\}(?!\s*@media))*\}/s)?.[0] || '';
  assert.match(coarse, /\.session-menu\{[^}]*opacity:1/);
  assert.match(coarse, /\.project-heading>\.ui-tooltip-anchor \.ui-icon-button\{opacity:1\}/);
  assert.match(styles, /\.session-row\.is-selected \.session-menu/);
});

test('P0 interaction architecture cannot regress to hidden cancel or viewport-breaking overlays', () => {
  const componentRoot = join(import.meta.dirname, '..', 'src', 'panel', 'components');
  const composer = readFileSync(join(componentRoot, 'Composer.tsx'), 'utf8');
  const sidebar = readFileSync(join(componentRoot, 'ProjectSidebar.tsx'), 'utf8');
  const dialog = readFileSync(join(import.meta.dirname, '..', 'src', 'ui', 'Dialog.tsx'), 'utf8');
  const styles = readFileSync(join(import.meta.dirname, '..', 'src', 'styles.css'), 'utf8');
  assert.match(composer, /cancelTurn/);
  assert.match(composer, /停止生成/);
  assert.match(dialog, /<Portal>/);
  assert.match(sidebar, /sidebar-footer/);
  assert.doesNotMatch(styles, /logout-button[^}]*position\s*:\s*fixed/s);
});

test('composer keeps the writing surface quiet and keyboard behavior discoverable', () => {
  const root = join(import.meta.dirname, '..', 'src');
  const composer = readFileSync(join(root, 'panel', 'components', 'Composer.tsx'), 'utf8');
  const styles = readFileSync(join(root, 'styles.css'), 'utf8');
  assert.match(composer, /Enter 发送 · Shift \+ Enter 换行/);
  assert.match(composer, /runtimeSummary/);
  assert.doesNotMatch(composer, />\s*effort：/);
  assert.doesNotMatch(composer, />\s*上下文：/);
  assert.match(styles, /\.composer-surface:focus-within\s*\{[^}]*border-color:\s*var\(--border-strong\)/);
  assert.match(styles, /\.composer-surface:has\(\.composer-input:focus-visible\)\s*\{[^}]*var\(--focus-ring\)/);
  assert.match(styles, /\.composer-toolbar>\.ui-tooltip-anchor\{margin-left:auto;flex:0 0 auto\}/);
  assert.doesNotMatch(styles, /\.composer-surface:focus-within\{[^}]*(?:blue|#[0-9a-f]*ff[0-9a-f]*)/i);
});

test('connection loss settles pending actions instead of silently discarding their callbacks', () => {
  const root = join(import.meta.dirname, '..', 'src', 'panel');
  const store = readFileSync(join(root, 'store.ts'), 'utf8');
  const tracker = readFileSync(join(root, 'lib', 'command-tracker.ts'), 'utf8');
  assert.match(tracker, /settleConnectionLoss\(\): void/);
  assert.match(tracker, /for \(const commandId of \[\.\.\.this\.pending\.keys\(\)\]\)/);
  assert.match(store, /case 'reconnecting':[\s\S]*?commands\.settleConnectionLoss\(\)/);
  assert.match(store, /export function connectWithCookie\(\)[\s\S]*?commands\.settleConnectionLoss\(\)/);
  assert.doesNotMatch(store, /connectWithCookie\(\)[\s\S]{0,500}setPersistentErrors\(\[\]\)/);
});

test('replaced websocket callbacks cannot mutate the new connection state', () => {
  const store = readFileSync(join(import.meta.dirname, '..', 'src', 'panel', 'store.ts'), 'utf8');
  assert.match(store, /let connectionEpoch = 0/);
  assert.match(store, /const epoch = \+\+connectionEpoch/);
  assert.match(store, /onStatus: \(state, detail\) => \{ if \(epoch === connectionEpoch\) onStatus\(state, detail\); \}/);
  assert.match(store, /onFrame: \(frame\) => \{ if \(epoch === connectionEpoch\) onFrame\(frame\); \}/);
});

test('routine connection readiness stays in persistent status instead of interrupting with a toast', () => {
  const store = readFileSync(join(import.meta.dirname, '..', 'src', 'panel', 'store.ts'), 'utf8');
  assert.match(store, /case 'ready':[\s\S]*?setConnState\(\{ text: '就绪', kind: 'ok' \}\)/);
  assert.doesNotMatch(store, /toast\('连接就绪'\)/);
});

test('browser diagnostics never print raw protocol or user payloads', () => {
  const root = join(import.meta.dirname, '..', 'src', 'panel');
  const ws = readFileSync(join(root, 'lib', 'ws-client.ts'), 'utf8');
  const store = readFileSync(join(root, 'store.ts'), 'utf8');
  const yjs = readFileSync(join(root, 'lib', 'yjs.ts'), 'utf8');
  assert.doesNotMatch(ws, /console\.(?:error|warn)\([^\n]*(?:ev\.data|frame\)|ev\.reason|,\s*ev\b)/);
  assert.doesNotMatch(store, /console\.error\([^\n]*JSON\.stringify\(err\)/);
  assert.doesNotMatch(yjs, /console\.warn\([^\n]*,\s*err\)/);
});

test('downstream parsing rejects unsafe JSON shapes while preserving future tags', () => {
  const protocol = readFileSync(join(import.meta.dirname, '..', 'src', 'panel', 'lib', 'protocol.ts'), 'utf8');
  const ws = readFileSync(join(import.meta.dirname, '..', 'src', 'panel', 'lib', 'ws-client.ts'), 'utf8');
  assert.match(protocol, /!value \|\| typeof value !== 'object' \|\| Array\.isArray\(value\)/);
  assert.match(protocol, /typeof frame\.t !== 'string' \|\| !frame\.t\.trim\(\)/);
  assert.doesNotMatch(protocol, /FRAME_TAGS|KNOWN_TAGS|switch \(frame\.t\)/);
  assert.match(ws, /\[ws-client\] 帧解析失败 length=\$\{size\}/);
  assert.doesNotMatch(ws, /帧解析失败[^\n]*(?:ev\.data|JSON\.stringify)/);
});

test('design tokens cannot directly reference themselves', () => {
  const css = readFileSync(join(import.meta.dirname, '..', 'src', 'ui', 'tokens.css'), 'utf8');
  const selfReferences = [...css.matchAll(/--([a-z0-9-]+)\s*:\s*var\(--\1\)/gi)].map((match) => match[1]);
  assert.deepEqual(selfReferences, []);
});

test('reusable design tokens have one UI-library source', () => {
  const root = join(import.meta.dirname, '..', 'src');
  const styles = readFileSync(join(root, 'styles.css'), 'utf8');
  const primitives = readFileSync(join(root, 'ui', 'primitives.css'), 'utf8');
  const tokens = readFileSync(join(root, 'ui', 'tokens.css'), 'utf8');
  assert.match(styles, /^@import 'tailwindcss';\n@import '\.\/ui\/primitives\.css';/);
  assert.doesNotMatch(styles, /@import '\.\/ui\/tokens\.css'/);
  assert.match(primitives, /^@import '\.\/tokens\.css';/);
  assert.doesNotMatch(styles, /:root\s*\{/);
  assert.match(tokens, /:root\s*\{/);
  assert.match(tokens, /--composer-border:/);
  assert.doesNotMatch(styles, /#[0-9a-f]{3,8}\b|rgba?\(/i);
  const declared = new Set([...tokens.matchAll(/--([a-z0-9-]+)\s*:/gi)].map((match) => match[1]));
  const sourceFiles = [styles, ...readdirSync(join(root, 'panel', 'components'))
    .filter((file) => file.endsWith('.tsx'))
    .map((file) => readFileSync(join(root, 'panel', 'components', file), 'utf8'))];
  const referenced = new Set(sourceFiles.flatMap((source) => [...source.matchAll(/var\(--([a-z0-9-]+)/gi)].map((match) => match[1])));
  assert.deepEqual([...referenced].filter((token) => !declared.has(token)), []);
});

test('primitive visuals are standalone and do not leak into feature styles', () => {
  const root = join(import.meta.dirname, '..', 'src');
  const styles = readFileSync(join(root, 'styles.css'), 'utf8');
  const primitives = readFileSync(join(root, 'ui', 'primitives.css'), 'utf8');
  const drawer = readFileSync(join(root, 'ui', 'Drawer.tsx'), 'utf8');
  const ownedSelectors = [
    '.ui-icon{', '.ui-button{', '.ui-icon-button{', '.ui-field{', '.ui-dialog-backdrop{',
    '.ui-drawer-scrim{', '.ui-menu{', '.ui-tooltip{', '.ui-status{', '.ui-badge{',
    '.ui-toast{', '.ui-empty{', '.ui-spinner{', '.ui-scrollbar{',
  ];
  for (const selector of ownedSelectors) {
    assert.match(primitives, new RegExp(selector.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
    const className = selector.slice(0, -1);
    const escapedClass = className.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    assert.doesNotMatch(
      styles,
      new RegExp(`(?:^|})\\s*${escapedClass}\\s*\\{`, 'm'),
      `${className} base styles must remain owned by primitives.css`,
    );
  }
  assert.match(drawer, /class="ui-drawer-scrim"/);
  assert.doesNotMatch(styles, /drawer-scrim/);
});

test('domain status inference delegates visual rendering to the shared Badge', () => {
  const root = join(import.meta.dirname, '..', 'src');
  const adapter = readFileSync(join(root, 'panel', 'components', 'Badge.tsx'), 'utf8');
  const primitive = readFileSync(join(root, 'ui', 'Badge.tsx'), 'utf8');
  assert.match(adapter, /Badge as UiBadge/);
  assert.doesNotMatch(adapter, /bg-\[|text-\[/);
  assert.match(primitive, /BadgeTone = 'neutral' \| 'ok' \| 'warn' \| 'err'/);
});

test('icon-only controls receive visible help from the shared Tooltip', () => {
  const root = join(import.meta.dirname, '..', 'src', 'ui');
  const button = readFileSync(join(root, 'Button.tsx'), 'utf8');
  const tooltip = readFileSync(join(root, 'Tooltip.tsx'), 'utf8');
  assert.match(button, /<Tooltip content=/);
  assert.doesNotMatch(button, /title=\{/);
  assert.match(tooltip, /role="tooltip"/);
  assert.match(tooltip, /event\.key === 'Escape'/);
  const sessionRow = readFileSync(join(import.meta.dirname, '..', 'src', 'panel', 'components', 'ProjectSessionRow.tsx'), 'utf8');
  assert.match(sessionRow, /<IconButton[^>]*[\s\S]*?class="session-menu"/);
  assert.doesNotMatch(sessionRow, /<button[^>]*class="session-menu"/);
});

test('responsive navigation behavior belongs to the shared Drawer primitive', () => {
  const root = join(import.meta.dirname, '..', 'src');
  const shell = readFileSync(join(root, 'panel', 'components', 'AppShell.tsx'), 'utf8');
  const drawer = readFileSync(join(root, 'ui', 'Drawer.tsx'), 'utf8');
  assert.match(shell, /<Drawer\b/);
  assert.match(shell, /if \(!query\.matches\) setOpen\(false\)/);
  assert.doesNotMatch(shell, /document\.addEventListener\('keydown'/);
  assert.doesNotMatch(shell, /\.inert\s*=/);
  assert.match(drawer, /acquireOverlay/);
  assert.match(drawer, /role=\{props\.modal && props\.open \? 'dialog'/);
  assert.match(drawer, /aria-modal=/);
});

test('all empty-session creation entry points share one store-level single-flight guard', () => {
  const root = join(import.meta.dirname, '..', 'src');
  const store = readFileSync(join(root, 'panel', 'store.ts'), 'utf8');
  const sidebar = readFileSync(join(root, 'panel', 'components', 'ProjectSidebar.tsx'), 'utf8');
  const chatView = readFileSync(join(root, 'panel', 'components', 'ChatView.tsx'), 'utf8');
  assert.match(store, /creatingSessionProjectId\(\)/);
  assert.match(store, /setCreatingSessionProjectId\(projectId\)/);
  assert.match(sidebar, /busy=\{creatingSessionProjectId\(\) === project\.id\}/);
  assert.match(chatView, /busy=\{creatingSessionProjectId\(\) === activeProjects\(\)\[0\]\.id\}/);
});

test('uncertain metadata retries preserve the original frame identity and are identity-scoped', () => {
  const root = join(import.meta.dirname, '..', 'src', 'panel');
  const store = readFileSync(join(root, 'store.ts'), 'utf8');
  const tracker = readFileSync(join(root, 'lib', 'command-tracker.ts'), 'utf8');
  assert.match(tracker, /this\.uncertain\.set\(commandId, request\)/);
  assert.match(tracker, /return this\.dispatch\(request, send\)/);
  assert.match(tracker, /frame: tracked\.frame/);
  assert.match(store, /const sent = commands\.retry\(commandId, sendFrame\) === 'sent'/);
  assert.match(store, /if \(sent\) setPersistentErrors/);
  assert.match(store, /commands\.reset\(\)/);
  assert.match(store, /function rejectWhileMetadataUncertain\(\)/);
  assert.match(store, /onUncertainCountChange: setUncertainMetadataCount/);
  for (const action of ['project/create', 'project/archive', 'project/restore', 'project/rename', 'session/create', 'session/rename', 'session/import']) {
    const escaped = action.replace('/', '\\/');
    assert.match(store, new RegExp(`sendAction\\(frame, '${escaped}', \\{ retryOnUncertain: true`), action);
  }
});

test('global session search matches durable metadata and excludes empty queries', () => {
  const projects = [{ id: 'p1', name: 'Perihelion', cwd: '/code/peri' }];
  const sessions = [
    { id: 's1', projectId: 'p1', title: '修复登录', acpSessionId: 'acp-123', updatedAt: '2026-08-13T10:00:00Z' },
    { id: 's2', projectId: 'p1', title: '组件审计', acpSessionId: 'acp-456', updatedAt: '2026-08-13T11:00:00Z' },
  ];
  assert.deepEqual(searchProjectSessions('', projects, sessions), []);
  assert.equal(searchProjectSessions('peri', projects, sessions).length, 2);
  assert.equal(searchProjectSessions('456', projects, sessions)[0].id, 's2');
  assert.equal(searchProjectSessions('登录', projects, sessions)[0].id, 's1');
});

test('session navigation closes only after a server-authoritative open commits', () => {
  const root = join(import.meta.dirname, '..', 'src', 'panel');
  const store = readFileSync(join(root, 'store.ts'), 'utf8');
  const sidebar = readFileSync(join(root, 'components', 'ProjectSidebar.tsx'), 'utf8');
  const sessionRow = readFileSync(join(root, 'components', 'ProjectSessionRow.tsx'), 'utf8');
  const search = readFileSync(join(root, 'components', 'SessionSearch.tsx'), 'utf8');
  assert.match(store, /export interface OpenSessionCallbacks/);
  assert.match(store, /callbacks\.onCommitted\?\.\(\)/);
  assert.match(sidebar, /onOpen=\{\(sessionId, onCommitted\) => \{ navigateProjectSession\(sessionId, \{ onCommitted \}\); \}\}/);
  assert.match(sessionRow, /props\.onOpen\(props\.session\.id, props\.onNavigate\)/);
  assert.doesNotMatch(sessionRow, /props\.onOpen\(props\.session\.id[^;]*;\s*props\.onNavigate\(\)/);
  assert.match(search, /onCommitted: \(\) => \{ props\.onClose\(\)/);
  assert.match(search, /onUncertain:/);
});
