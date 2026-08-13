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
import { Show } from 'solid-js';
import { selectedSessionId } from '../store';
import { EmptyState } from '../../ui/EmptyState';

type ChatViewProps = {
  onOpenNavigation?: () => void;
  onOpenStatus?: () => void;
};

export function ChatView(props: ChatViewProps) {
  return (
    <section class="flex h-full min-h-0 flex-col">
      <ChatHeader
        onOpenNavigation={props.onOpenNavigation}
        onOpenStatus={props.onOpenStatus}
      />
      <Show when={selectedSessionId()} fallback={<EmptyState title="今天想做什么？" description="从左侧项目中新建或打开一个会话。acp-hub 会恢复对应的 ACP 上下文。" />}>
        <MessageList />
        <Composer />
      </Show>
    </section>
  );
}
