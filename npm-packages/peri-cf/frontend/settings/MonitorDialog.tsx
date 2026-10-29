import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { Activity, Bot, Cpu, Plug, RefreshCw, ShieldCheck, X } from "lucide-react";
import { useFocusTrap, useMediaQuery } from "@/hooks";
import { createHttpMonitorRepository, type MonitorRepository } from "../remote";
import { formatClock } from "./monitor/format";
import { InstanceSection } from "./monitor/InstanceSection";
import { LlmSection } from "./monitor/LlmSection";
import { McpSection } from "./monitor/McpSection";
import { SandboxSection } from "./monitor/SandboxSection";
import {
  MONITOR_POLL_INTERVAL_MS,
  useMonitorSnapshot,
} from "./monitor/use-monitor-snapshot";

/** 与 CSS `max-width: 760px` 使用同一断点：761px 起为竖向 tab。 */
const VERTICAL_TABS_MEDIA_QUERY = "(min-width: 761px)";
const TAB_KEYS = ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight", "Home", "End"];

type MonitorTab = "llm" | "mcp" | "instances" | "sandbox";

const TABS: ReadonlyArray<{ id: MonitorTab; label: string; Icon: typeof Activity }> = [
  { id: "llm", label: "LLM 配置", Icon: Bot },
  { id: "mcp", label: "MCP 配置", Icon: Plug },
  { id: "instances", label: "WASM 实例", Icon: Cpu },
  { id: "sandbox", label: "Sandbox 情况", Icon: ShieldCheck },
];

export interface MonitorDialogProps {
  onClose: () => void;
  /** 测试注入；默认走 `GET /api/monitor`。 */
  repository?: MonitorRepository;
  intervalMs?: number;
}

/**
 * 服务端监控：macOS 系统设置式左 tab / 右详情，打开期间轮询。
 *
 * 面板只读服务端已去敏的快照；读取不会启动 sandbox、WASM 运行后端或 Agent。
 */
export function MonitorDialog({ onClose, repository, intervalMs }: MonitorDialogProps) {
  const fallbackRepository = useMemo(() => createHttpMonitorRepository(), []);
  const { snapshot, updatedAt, error, loading, refresh } = useMonitorSnapshot(
    repository ?? fallbackRepository,
    intervalMs ?? MONITOR_POLL_INTERVAL_MS,
  );
  const [tab, setTab] = useState<MonitorTab>("llm");
  const cardRef = useRef<HTMLDivElement>(null);
  const tabRefs = useRef(new Map<MonitorTab, HTMLButtonElement>());
  const vertical = useMediaQuery(VERTICAL_TABS_MEDIA_QUERY);

  // 记录打开前的焦点；卸载（关闭）时归还给触发按钮。
  useEffect(() => {
    const previous = typeof document === "undefined" ? null : document.activeElement;
    return () => {
      if (previous instanceof HTMLElement) previous.focus();
    };
  }, []);
  useFocusTrap(true, cardRef);

  // 打开时焦点落在当前 tab；useFocusTrap 已先把焦点移入容器。
  useEffect(() => {
    tabRefs.current.get("llm")?.focus();
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  const active = TABS.find((item) => item.id === tab) ?? TABS[0]!;

  const onTabKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    if (!TAB_KEYS.includes(event.key)) return;
    event.preventDefault();
    const current = TABS.findIndex((item) => item.id === tab);
    const next = event.key === "Home" ? 0
      : event.key === "End" ? TABS.length - 1
        : event.key === "ArrowUp" || event.key === "ArrowLeft"
          ? (current - 1 + TABS.length) % TABS.length
          : (current + 1) % TABS.length;
    const target = TABS[next]!;
    setTab(target.id);
    tabRefs.current.get(target.id)?.focus();
  };

  const statusText = snapshot === null
    ? (loading ? "正在读取…" : "暂无数据")
    : `更新于 ${formatClock(updatedAt)}`;

  return (
    <div className="settings-shell">
      <div
        ref={cardRef}
        className="settings-card monitor-card"
        role="dialog"
        aria-modal="true"
        aria-labelledby="monitor-title"
      >
        <header className="monitor-head">
          <span className="brand-mark"><Activity size={19} /></span>
          <h1 id="monitor-title">服务端监控</h1>
          <span className="monitor-updated" role="status" aria-live="polite">{statusText}</span>
          <button
            type="button"
            className="icon-button"
            aria-label="刷新监控数据"
            title="刷新"
            onClick={refresh}
            disabled={loading && snapshot === null}
          >
            <RefreshCw size={16} className={loading ? "spinning" : undefined} />
          </button>
          <button
            type="button"
            className="icon-button"
            aria-label="关闭监控面板"
            title="关闭"
            onClick={onClose}
          >
            <X size={17} />
          </button>
        </header>

        {error && (
          <div className="monitor-alert" role="alert">
            <span>
              {snapshot === null
                ? `读取失败：${error}`
                : `更新失败（上次更新 ${formatClock(updatedAt)}）：${error}（显示上次数据）`}
            </span>
            <button type="button" className="text-button" onClick={refresh}>重试</button>
          </div>
        )}

        <div className="monitor-body">
          <div
            role="tablist"
            aria-label="监控分类"
            aria-orientation={vertical ? "vertical" : "horizontal"}
            className="monitor-tabs"
            onKeyDown={onTabKeyDown}
          >
            {TABS.map(({ id, label, Icon }) => (
              <button
                key={id}
                ref={(node) => {
                  if (node) tabRefs.current.set(id, node);
                  else tabRefs.current.delete(id);
                }}
                type="button"
                role="tab"
                id={`monitor-tab-${id}`}
                aria-controls={`monitor-panel-${id}`}
                aria-selected={tab === id}
                tabIndex={tab === id ? 0 : -1}
                className="monitor-tab"
                onClick={() => setTab(id)}
              >
                <Icon size={16} />
                <span>{label}</span>
              </button>
            ))}
          </div>

          <div
            role="tabpanel"
            id={`monitor-panel-${active.id}`}
            aria-labelledby={`monitor-tab-${active.id}`}
            tabIndex={0}
            className="monitor-panel"
          >
            {snapshot === null ? (
              <p className="settings-intro" role="status">
                {loading ? "正在读取监控数据…" : "暂无监控数据，请重试。"}
              </p>
            ) : active.id === "llm" ? (
              <LlmSection llm={snapshot.llm} />
            ) : active.id === "mcp" ? (
              <McpSection mcp={snapshot.mcp} />
            ) : active.id === "instances" ? (
              <InstanceSection wasm={snapshot.wasm} generatedAt={snapshot.generatedAt} />
            ) : (
              <SandboxSection sandbox={snapshot.sandbox} generatedAt={snapshot.generatedAt} />
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
