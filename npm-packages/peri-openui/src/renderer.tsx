// devtools 必须在 react-lang 求值前设置内部开关。
import "./devtools-off";
import type { ElementNode } from "@openuidev/lang-core";
import { createParser, Renderer } from "@openuidev/react-lang";
import { useMemo, type ReactNode } from "react";
import type { OpenUIActionEvent } from "./action-event";
import { OpenUIActionEnabledContext } from "./components/shared";
import { openuiLibrary } from "./library";

export { OpenUIErrorBoundary } from "./openui-error-boundary";

const parser = createParser(openuiLibrary.toJSONSchema(), openuiLibrary.root);

function isElementNode(value: unknown): value is ElementNode {
  return value !== null && typeof value === "object"
    && (value as { type?: unknown }).type === "element";
}

function isActionExpression(value: unknown): boolean {
  return value !== null && typeof value === "object";
}

/**
 * parser 已报告的 meta.errors/unresolved 之外，JSON Schema 校验对
 * `string | number` 这类联合类型不够严格（例如 `Text(true)` 会被放行）。
 * 再用 zod 递归校验一次，避免 Renderer 静默留下空白节点。
 */
function hasInvalidElement(node: ElementNode): boolean {
  const component = openuiLibrary.components[node.typeName];
  if (!component) return true;

  // Button 的 action 在解析阶段是 AST、渲染阶段是 ActionPlan；zod schema 描述的是
  // legacy action 对象。只要 action 存在，就用等价合法值校验其余字段；缺失仍会失败。
  const props = node.typeName === "Button" && isActionExpression(node.props.action)
    ? { ...node.props, action: { type: "continue_conversation" } }
    : node.props;

  if (!component.props.safeParse(props).success) return true;

  for (const value of Object.values(node.props)) {
    if (Array.isArray(value)) {
      if (value.some((child) => isElementNode(child) && hasInvalidElement(child))) return true;
    } else if (isElementNode(value) && hasInvalidElement(value)) {
      return true;
    }
  }
  return false;
}

export interface OpenUIProgramProps {
  readonly program: string;
  readonly isStreaming: boolean;
  readonly actionsEnabled?: boolean;
  readonly before?: string;
  readonly after?: string;
  /** 整条消息的原文，解析失败时完整回退。 */
  readonly fallbackText: string;
  /** 宿主注入的文本渲染器（聊天侧传 StreamdownText）；缺省按纯文本输出。 */
  readonly renderText?: (text: string) => ReactNode;
  readonly onAction?: (event: OpenUIActionEvent) => void;
}

function warningFallback(kind: string, fallback: ReactNode): ReactNode {
  return <div data-openui-fallback={kind}>{fallback}</div>;
}

function warnRendererErrors(errors: unknown[]): void {
  const env = (import.meta as unknown as { env?: { DEV?: boolean } }).env;
  if (env?.DEV) console.warn("[openui]", errors);
}

/**
 * 重 chunk：parser + library + Renderer。
 * 只有 OpenUIMessage 的懒加载入口会 import 本文件；主 chunk 不应包含它。
 */
const renderPlainText = (text: string): ReactNode => text;

export function OpenUIProgram({
  program,
  isStreaming,
  actionsEnabled = true,
  before = "",
  after = "",
  fallbackText,
  renderText = renderPlainText,
  onAction,
}: OpenUIProgramProps) {
  const parsed = useMemo(() => {
    try {
      return parser.parse(program);
    } catch {
      return null;
    }
  }, [program]);

  const invalid = useMemo(
    () => (parsed?.root ? hasInvalidElement(parsed.root) : false),
    [parsed],
  );

  if (!parsed?.root) {
    return warningFallback("parse-failed", renderText(fallbackText));
  }
  if (parsed.meta.incomplete) {
    return warningFallback("incomplete-program", renderText(fallbackText));
  }
  // root 存在但存在校验错误/未解析引用时，Renderer 会静默丢弃节点并留下空白。
  // 这类残缺 UI 不可接受：回退整段原文并标记原因（orphaned 不影响渲染，不触发）。
  if (parsed.meta.errors.length > 0 || parsed.meta.unresolved.length > 0 || invalid) {
    return warningFallback("validation-failed", renderText(fallbackText));
  }

  const interactionEnabled = actionsEnabled && !isStreaming;
  return (
    <OpenUIActionEnabledContext.Provider value={interactionEnabled}>
      {before ? renderText(before) : null}
      <div data-slot="peri-openui" style={{ minWidth: 0 }}>
        <Renderer
          response={program}
          library={openuiLibrary}
          isStreaming={isStreaming}
          onAction={onAction}
          publishObservability={false}
          onError={warnRendererErrors}
        />
      </div>
      {after ? renderText(after) : null}
    </OpenUIActionEnabledContext.Provider>
  );
}
