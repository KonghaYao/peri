import { useContext } from 'react';
import { UNSAFE_DataRouterContext, useBlocker } from 'react-router';
import { ConfirmDialog } from '@/components/confirm-dialog';

function RouterGuard({ dirty }: { readonly dirty: boolean }) {
  const blocker = useBlocker(dirty);
  return (
    <ConfirmDialog
      open={blocker.state === 'blocked'}
      onOpenChange={(open) => {
        if (!open && blocker.state === 'blocked') blocker.reset();
      }}
      title="离开未保存的草稿"
      description="当前修改尚未保存，离开将丢失这些修改。"
      confirmLabel="放弃并离开"
      onConfirm={() => {
        if (blocker.state === 'blocked') blocker.proceed();
      }}
    />
  );
}

// MemoryRouter 组件测试不具备 data router；生产 BrowserRouter 始终使用同一阻拦路径。
export function DraftNavigationGuard({ dirty }: { readonly dirty: boolean }) {
  const router = useContext(UNSAFE_DataRouterContext);
  return router ? <RouterGuard dirty={dirty} /> : null;
}
