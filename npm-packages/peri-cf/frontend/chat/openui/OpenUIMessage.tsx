import { lazy, Suspense, useMemo, type ReactNode } from "react";
import { splitOpenUI, type OpenUIActionEvent } from "peri-openui";
import { OpenUIErrorBoundary } from "peri-openui/error-boundary";

const LazyOpenUIProgram = lazy(() =>
  import("peri-openui/renderer").then((module) => ({ default: module.OpenUIProgram })),
);

export interface OpenUIMessageProps {
  readonly text: string;
  readonly streaming: boolean;
  readonly renderMarkdown: (text: string) => ReactNode;
  readonly onOpenUIAction?: (event: OpenUIActionEvent) => void;
  readonly actionsEnabled?: boolean;
}

function Skeleton({ label }: { label: string }) {
  return (
    <div data-slot="peri-openui-skeleton" role="status" aria-live="polite">
      {label}
    </div>
  );
}

/**
 * 轻量门面：只 eager 引入 fence/类型/错误边界；canvas 与 react-lang 全部在
 * `peri-openui/renderer` 的懒加载 chunk 中。
 */
export function OpenUIMessage({
  text,
  streaming,
  renderMarkdown,
  onOpenUIAction,
  actionsEnabled = true,
}: OpenUIMessageProps) {
  const doc = useMemo(() => splitOpenUI(text, streaming), [text, streaming]);

  if (doc.program === null) {
    if (doc.status === "open" && streaming) {
      return (
        <>
          {doc.before ? renderMarkdown(doc.before) : null}
          <Skeleton label="正在生成界面…" />
          {doc.after ? renderMarkdown(doc.after) : null}
        </>
      );
    }
    if (doc.status === "open" && !streaming) {
      return <div data-openui-fallback="incomplete-fence">{renderMarkdown(text)}</div>;
    }
    // 普通 Markdown、多围栏、流式裸 DSL 都保持原文渲染。
    return <>{renderMarkdown(text)}</>;
  }

  return (
    <OpenUIErrorBoundary
      key={doc.program}
      resetKey={doc.program}
      fallback={renderMarkdown(text)}
    >
      <Suspense
        fallback={
          <>
            {doc.before ? renderMarkdown(doc.before) : null}
            <Skeleton label="正在渲染界面…" />
            {doc.after ? renderMarkdown(doc.after) : null}
          </>
        }
      >
        <LazyOpenUIProgram
          program={doc.program}
          isStreaming={streaming}
          actionsEnabled={actionsEnabled}
          before={doc.before}
          after={doc.after}
          fallbackText={text}
          renderText={renderMarkdown}
          onAction={onOpenUIAction}
        />
      </Suspense>
    </OpenUIErrorBoundary>
  );
}
