// 右区组装层：对话头部（ChatHeader）+ 消息区（MessageList）+ 发送窗口
// （Composer）。三个子组件各自独立文件，分别承载对话区功能演进：
//   - ChatHeader：title 点击 → ACP 会话列表 tooltip；对话操作 icon 收纳
//   - MessageList：reasoning 在前/正文在后；loading 状态组件
//   - Composer：模型/effort/上下文占用显示；session/new 新会话按钮
//
// 三段式 flex column（ui.md §四.4）：ChatHeader 固定顶部、MessageList
// flex-1 + min-h-0 承担滚动、Composer 固定在底部。onOpenNavigation /
// onOpenStatus 是纯 UI seam（ui.md §四.5，不涉及协议），透传给 ChatHeader
// 供中窄屏打开左右 drawer。

import { ChatHeader } from './ChatHeader';
import { Composer } from './Composer';
import { MessageList } from './MessageList';
import { createMemo, Show } from 'solid-js';
import { createProjectSession, creatingSessionProjectId, projects, readOnly, selectedSessionId } from '../store';
import { EmptyState } from '../../ui';
import { ConnectionProblem } from './ConnectionProblem';
import { restoringSessionId } from '../store';
import { ErrorCenter } from './ErrorCenter';
import { Button } from '../../ui';
import { QuickStartComposer } from './QuickStartComposer';

type ChatViewProps = {
  onOpenNavigation?: () => void;
  onOpenStatus?: () => void;
  onCreateProject?: () => void;
  onImport?: (projectId: string) => void;
};

export function ChatView(props: ChatViewProps) {
  const activeProjects = createMemo(() => projects().filter((project) => !project.archivedAt));
  return (
    <section class="flex h-full min-h-0 flex-col">
      <ChatHeader
        onOpenNavigation={props.onOpenNavigation}
        onOpenStatus={props.onOpenStatus}
      />
      <ConnectionProblem />
      <ErrorCenter />
      <Show when={restoringSessionId()}><div class="restore-banner" role="status"><span class="ui-spinner" aria-hidden="true" />正在恢复上次会话与 ACP 上下文…</div></Show>
      <Show when={selectedSessionId()} fallback={<EmptyState title="今天想做什么？" description={activeProjects().length ? '新建一个会话，或将已有 ACP 会话加入侧边栏。' : '先创建一个项目，acp-hub 会在其中保存并恢复 ACP 会话。'} action={
        <div class="empty-actions">
          <Show when={activeProjects().length === 0}><Button variant="primary" disabled={readOnly()} onClick={props.onCreateProject}>新建项目</Button></Show>
          <Show when={activeProjects().length > 0}><QuickStartComposer projects={activeProjects().map(({ id, name }) => ({ id, name }))} initialProjectId={activeProjects().length === 1 ? activeProjects()[0].id : undefined} /><div class="empty-secondary-actions"><Show when={activeProjects().length === 1}><Button busy={creatingSessionProjectId() === activeProjects()[0].id} disabled={readOnly() || !!creatingSessionProjectId()} onClick={() => createProjectSession(activeProjects()[0].id)}>先建空会话</Button><Button disabled={readOnly()} onClick={() => props.onImport?.(activeProjects()[0].id)}>导入已有会话</Button></Show><Show when={activeProjects().length > 1}><Button onClick={props.onOpenNavigation}>浏览项目与会话</Button></Show></div></Show>
        </div>
      } />}>
        <MessageList />
        <Composer />
      </Show>
    </section>
  );
}
