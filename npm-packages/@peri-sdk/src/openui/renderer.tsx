// devtools 必须在 react-lang 求值前设置内部开关。
import "./devtools-off";
import type { ElementNode, ParseResult } from "@openuidev/lang-core";
import { createParser, Renderer } from "@openuidev/react-lang";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { OpenUIActionEvent } from "./action-event";
import { OpenUIActionEnabledContext } from "./components/shared";
import { openuiLibrary } from "./library";

export { OpenUIErrorBoundary } from "./openui-error-boundary";

/** 流式解析/渲染最多每 120ms 推进一次，避免逐字符 parse。 */
const STREAM_THROTTLE_MS = 120;

/**
 * 解析诊断：供流式节流测试与探针读取，不参与渲染决策。
 * `count` 只统计本模块发起的 parse 次数。
 */
export const openuiParseStats = {
  count: 0,
  maxDurationMs: 0,
};

function nowMs(): number {
  return typeof performance === "undefined" ? Date.now() : performance.now();
}

/**
 * 复用同一个 parser 会累积内部状态，流式前缀必须每次新建。
 */
function parseProgram(program: string): ParseResult | null {
  const started = nowMs();
  try {
    return createParser(openuiLibrary.toJSONSchema(), openuiLibrary.root).parse(program);
  } catch {
    return null;
  } finally {
    openuiParseStats.count += 1;
    openuiParseStats.maxDurationMs = Math.max(openuiParseStats.maxDurationMs, nowMs() - started);
  }
}

/**
 * leading + trailing 节流：首次立即生效，窗口内的更新合并为最后一次。
 * 非流式直接返回原值，保证严格模式的同步校验。
 */
function useThrottledProgram(program: string, isStreaming: boolean): string {
  const [snapshot, setSnapshot] = useState(program);
  const latestRef = useRef(program);
  latestRef.current = program;
  const lastCommitRef = useRef(Date.now());
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const clearTimer = useCallback(() => {
    if (timerRef.current !== null) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  useEffect(() => clearTimer, [clearTimer]);

  useEffect(() => {
    if (!isStreaming) {
      clearTimer();
      lastCommitRef.current = 0;
      return;
    }
    if (program === snapshot) return;

    const elapsed = Date.now() - lastCommitRef.current;
    if (lastCommitRef.current === 0 || elapsed >= STREAM_THROTTLE_MS) {
      lastCommitRef.current = Date.now();
      setSnapshot(program);
      return;
    }
    if (timerRef.current !== null) return;
    timerRef.current = setTimeout(() => {
      timerRef.current = null;
      lastCommitRef.current = Date.now();
      setSnapshot(latestRef.current);
    }, STREAM_THROTTLE_MS - elapsed);
  }, [program, isStreaming, snapshot, clearTimer]);

  return isStreaming ? snapshot : program;
}

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

/**
 * 流式模式取“最近一次可解析”的程序：root 存在且无 parser 错误。
 * unresolved 允许（定义稍后到达）；瞬时错误期间继续展示上一版 UI。
 */
function resolveStreamingProgram(
  parsed: ParseResult | null,
  program: string,
  lastGood: { current: string | null },
): string | null {
  if (parsed?.root && parsed.meta.errors.length === 0) {
    lastGood.current = program;
    return program;
  }
  // 新一轮流式从空内容重新开始，旧结果不再适用。
  if (!program) lastGood.current = null;
  return lastGood.current;
}

function StreamingSkeleton() {
  return (
    <div data-slot="peri-openui-skeleton" role="status" aria-live="polite">
      正在生成界面…
    </div>
  );
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
  const activeProgram = useThrottledProgram(program, isStreaming);
  const parsed = useMemo(() => parseProgram(activeProgram), [activeProgram]);
  const lastGoodRef = useRef<string | null>(null);

  if (isStreaming) {
    const renderProgram = resolveStreamingProgram(parsed, activeProgram, lastGoodRef);
    return (
      <OpenUIActionEnabledContext.Provider value={false}>
        {before ? renderText(before) : null}
        {renderProgram === null ? (
          <StreamingSkeleton />
        ) : (
          <div data-slot="peri-openui" style={{ minWidth: 0 }}>
            <Renderer
              response={renderProgram}
              library={openuiLibrary}
              isStreaming
              onAction={onAction}
              publishObservability={false}
              onError={warnRendererErrors}
            />
          </div>
        )}
        {after ? renderText(after) : null}
      </OpenUIActionEnabledContext.Provider>
    );
  }

  if (!parsed?.root) {
    return warningFallback("parse-failed", renderText(fallbackText));
  }
  if (parsed.meta.incomplete) {
    return warningFallback("incomplete-program", renderText(fallbackText));
  }
  // root 存在但存在校验错误/未解析引用时，Renderer 会静默丢弃节点并留下空白。
  // 这类残缺 UI 不可接受：回退整段原文并标记原因（orphaned 不影响渲染，不触发）。
  if (
    parsed.meta.errors.length > 0
    || parsed.meta.unresolved.length > 0
    || hasInvalidElement(parsed.root)
  ) {
    return warningFallback("validation-failed", renderText(fallbackText));
  }

  const interactionEnabled = actionsEnabled;
  return (
    <OpenUIActionEnabledContext.Provider value={interactionEnabled}>
      {before ? renderText(before) : null}
      <div data-slot="peri-openui" style={{ minWidth: 0 }}>
        <Renderer
          response={program}
          library={openuiLibrary}
          isStreaming={false}
          onAction={onAction}
          publishObservability={false}
          onError={warnRendererErrors}
        />
      </div>
      {after ? renderText(after) : null}
    </OpenUIActionEnabledContext.Provider>
  );
}
