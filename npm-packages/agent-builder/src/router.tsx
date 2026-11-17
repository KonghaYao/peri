import { createBrowserRouter, Navigate } from 'react-router';
import { AppShell } from '@/components/app-shell';
import { PlaceholderPage } from '@/components/placeholder-page';
import { AgentCreatePage } from '@/features/agents/agent-create-page';
import { AgentDetailLayout } from '@/features/agents/agent-detail-layout';
import { AgentEditPage } from '@/features/agents/agent-edit-page';
import { AgentListPage } from '@/features/agents/agent-list-page';
import { AgentPreviewPage } from '@/features/agents/agent-preview-page';
import { AgentToolsPage } from '@/features/agents/agent-tools-page';
import { ChatListPage } from '@/features/chats/chat-list-page';
import { ChatNewPage } from '@/features/chats/chat-new-page';
import { ChatPage } from '@/features/chats/chat-page';
import { ProvidersPage } from '@/features/providers/providers-page';

export const router = createBrowserRouter([
  {
    path: '/',
    element: <AppShell />,
    children: [
      { index: true, element: <Navigate replace to="/agents" /> },
      {
        path: 'agents',
        children: [
          { index: true, element: <AgentListPage /> },
          { path: 'new', element: <AgentCreatePage /> },
          {
            path: ':agentId',
            element: <AgentDetailLayout />,
            children: [
              { index: true, element: <Navigate replace to="edit" /> },
              { path: 'edit', element: <AgentEditPage /> },
              { path: 'preview', element: <AgentPreviewPage /> },
              { path: 'tools', element: <AgentToolsPage /> },
            ],
          },
        ],
      },
      { path: 'providers', element: <ProvidersPage /> },
      {
        path: 'chats',
        children: [
          { index: true, element: <ChatListPage /> },
          { path: 'new', element: <ChatNewPage /> },
          { path: ':chatId', element: <ChatPage /> },
        ],
      },
      {
        path: '*',
        element: <PlaceholderPage title="页面不存在" description="请从左侧导航返回 Agents。" />,
      },
    ],
  },
]);
