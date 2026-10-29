import { lazy, Suspense, useMemo, type ReactNode } from "react";
import { splitOpenUI, type OpenUIActionEvent } from "@peri-code/sdk/openui";
import { OpenUIErrorBoundary } from "@peri-code/sdk/openui/error-boundary";

const LazyOpenUIProgram = lazy(() =>
  import("@peri-code/sdk/openui/renderer").then((module) => ({ default: module.OpenUIProgram })),
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
 * `@peri-code/sdk/openui/renderer` 的懒加载 chunk 中。
 */
export function OpenUIMessage({
  text,
  streaming,
  renderMarkdown,
  onOpenUIAction,
  actionsEnabled = true,
}: OpenUIMessageProps) {
  const doc = useMemo(() => splitOpenUI(text, streaming), [text, streaming]);

  // 流式未闭合：已到达的片段直接交给 React-lang 流式解析，不再等闭合围栏。
  if (doc.status === "open" && streaming) {
    if (!doc.program) {
      return (
        <>
          {doc.before ? renderMarkdown(doc.before) : null}
          <Skeleton label="正在生成界面…" />
        </>
      );
    }
    return (
      <OpenUIErrorBoundary
        resetKey={doc.program}
        fallback={<Skeleton label="正在生成界面…" />}
      >
        <Suspense
          fallback={
            <>
              {doc.before ? renderMarkdown(doc.before) : null}
              <Skeleton label="正在渲染界面…" />
            </>
          }
        >
          <LazyOpenUIProgram
            program={doc.program}
            isStreaming
            actionsEnabled={actionsEnabled}
            before={doc.before}
            // 未闭合围栏之后不可能有 Markdown；显式传空，避免流式期间渲染 after。
            after=""
            fallbackText={text}
            renderText={renderMarkdown}
            onAction={onOpenUIAction}
          />
        </Suspense>
      </OpenUIErrorBoundary>
    );
  }

  // 流结束仍未闭合：半截程序不可解析，回退原文并标记原因。
  if (doc.status === "open") {
    return <div data-openui-fallback="incomplete-fence">{renderMarkdown(text)}</div>;
  }

  // 普通 Markdown、多围栏、流式裸 DSL 都保持原文渲染。
  if (doc.program === null) return <>{renderMarkdown(text)}</>;

  // 闭合围栏与裸 DSL 走严格路径：完整校验后才挂载 Renderer。
  return (
    <OpenUIErrorBoundary resetKey={doc.program} fallback={renderMarkdown(text)}>
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
          isStreaming={false}
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
