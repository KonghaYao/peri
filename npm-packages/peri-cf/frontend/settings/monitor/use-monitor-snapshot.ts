import { useCallback, useEffect, useRef, useState } from "react";
import type { MonitorRepository, MonitorSnapshot } from "../../remote";

export const MONITOR_POLL_INTERVAL_MS = 5000;

export interface MonitorSnapshotState {
  snapshot: MonitorSnapshot | null;
  /** 最近一次成功读取的本地时间。 */
  updatedAt: Date | null;
  error: string;
  /** 首屏读取尚未落定。 */
  loading: boolean;
  refresh: () => void;
}

/**
 * 监控轮询：打开即读，之后按 `intervalMs` 刷新。
 *
 * - 单飞：上一请求未结束时跳过本轮；手动刷新同样不排队。
 * - 页面隐藏时暂停轮询；`visibilitychange` 回到可见立即刷新。
 * - 卸载时清理定时器并 abort 在途请求；序号守卫丢弃过期响应。
 * - 失败保留上次快照，只更新 `error`。
 */
export function useMonitorSnapshot(
  repository: MonitorRepository,
  intervalMs = MONITOR_POLL_INTERVAL_MS,
): MonitorSnapshotState {
  const [snapshot, setSnapshot] = useState<MonitorSnapshot | null>(null);
  const [updatedAt, setUpdatedAt] = useState<Date | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const inFlight = useRef<AbortController | null>(null);
  const sequence = useRef(0);
  const mounted = useRef(false);

  const read = useCallback(async (force: boolean) => {
    if (inFlight.current) return;
    if (!force && typeof document !== "undefined" && document.hidden) return;
    const controller = new AbortController();
    inFlight.current = controller;
    const ticket = ++sequence.current;
    try {
      const next = await repository.read(controller.signal);
      if (!mounted.current || ticket !== sequence.current) return;
      setSnapshot(next);
      setUpdatedAt(new Date());
      setError("");
    } catch (failure) {
      if (!mounted.current || ticket !== sequence.current || controller.signal.aborted) return;
      setError(failure instanceof Error ? failure.message : "监控数据读取失败。");
    } finally {
      if (inFlight.current === controller) inFlight.current = null;
      if (mounted.current && ticket === sequence.current) setLoading(false);
    }
  }, [repository]);

  useEffect(() => {
    mounted.current = true;
    // 打开即请求，即使页面此刻处于隐藏态；后续轮询遵循可见性。
    void read(true);
    const timer = setInterval(() => { void read(false); }, intervalMs);
    const onVisibility = () => {
      if (!document.hidden) void read(false);
    };
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      mounted.current = false;
      clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisibility);
      inFlight.current?.abort();
      inFlight.current = null;
    };
  }, [read, intervalMs]);

  const refresh = useCallback(() => { void read(true); }, [read]);
  return { snapshot, updatedAt, error, loading, refresh };
}
