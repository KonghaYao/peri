import type { WasmAcpTransport } from "../transport/wasm-transport";
import { withDeadline } from "../util/deadline";
import { DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS, type PeriWasmHostCleanupOutcome } from "./host";

/** 宿主启动器：只依赖取消信号，便于替换模块工厂或测试桩。 */
export type PeriWasmHostStarter = (options: { signal: AbortSignal }) => Promise<WasmAcpTransport>;

/** 单宿主状态：`live → closing → closed | close-unconfirmed`。 */
export type PeriWasmHostState = "live" | "closing" | "closed" | "close-unconfirmed";

/** 默认启动超时；与调用方既有预算保持一致。 */
export const DEFAULT_WASM_HOST_STARTUP_TIMEOUT_MS = 20_000;

export interface PeriWasmHostLeaseOptions {
  /** 启动超时；超时后中止启动并拒绝 `start`。 */
  startupTimeoutMs?: number;
  /** 关闭超时；超时或失败记为 `close-unconfirmed`。 */
  closeTimeoutMs?: number;
  /** 外部取消信号；释放实例或进程退出时中止启动。 */
  signal?: AbortSignal;
  /** 启动已判定失败、但迟到宿主仍被关闭时的证据回调。 */
  onLateCleanup?: (outcome: PeriWasmHostCleanupOutcome) => void;
}

export interface PeriWasmHostCloseReport extends PeriWasmHostCleanupOutcome {
  /** 宿主身份；启动成功后固定。 */
  generationId: string;
}

/**
 * 单个 WASM 宿主的所有权租约。
 * - `start` 统一启动、超时与取消，并负责关闭迟到宿主；
 * - `close` 幂等且不抛出，返回 `confirmed` 证据；
 * - 调用方只依据 `state`/`confirmed` 决定是否继续使用宿主。
 */
export class PeriWasmHostLease {
  /** 当前状态；`close-unconfirmed` 表示关闭超时或失败。 */
  private stateValue: PeriWasmHostState = "live";
  /** 关闭结算的幂等缓存。 */
  private closePromise: Promise<PeriWasmHostCloseReport> | undefined;

  /** 关闭超时；超时或失败记为 `close-unconfirmed`。 */
  private constructor(private readonly transportValue: WasmAcpTransport,
    private readonly closeTimeoutMs: number) {}

  /** 启动宿主；超时/取消后中止内部信号并保留原始错误。 */
  static async start(starter: PeriWasmHostStarter,
    options: PeriWasmHostLeaseOptions = {}): Promise<PeriWasmHostLease> {
    const controller = new AbortController();
    const abort = (): void => controller.abort(options.signal?.reason);
    if (options.signal?.aborted) abort();
    else options.signal?.addEventListener("abort", abort, { once: true });
    const closeTimeoutMs = options.closeTimeoutMs ?? DEFAULT_WASM_HOST_CLEANUP_TIMEOUT_MS;
    const starting = Promise.resolve().then(() => starter({ signal: controller.signal }));
    try {
      const transport = await withDeadline(
        starting,
        options.startupTimeoutMs ?? DEFAULT_WASM_HOST_STARTUP_TIMEOUT_MS,
        "Peri WASM host startup",
      );
      return new PeriWasmHostLease(transport, closeTimeoutMs);
    } catch (error) {
      controller.abort(error);
      // 启动可能仍会成功：迟到宿主必须关闭，否则所有权泄漏。
      void starting.then(async (late) => {
        try {
          await withDeadline(late.close(), closeTimeoutMs, "Peri WASM late host close");
          options.onLateCleanup?.({ confirmed: true });
        } catch (cleanupError) {
          options.onLateCleanup?.({ confirmed: false, error: cleanupError });
        }
      }, () => undefined);
      throw error;
    } finally {
      options.signal?.removeEventListener("abort", abort);
    }
  }

  /** 当前状态；关闭结果未确认时保持 `close-unconfirmed`。 */
  get state(): PeriWasmHostState { return this.stateValue; }
  /** 已启动宿主；仅在 `live`/`closing` 期间可发 ACP 请求。 */
  get transport(): WasmAcpTransport { return this.transportValue; }
  /** 宿主身份；日志与诊断使用。 */
  get generationId(): string { return this.transportValue.generationId; }

  /** 关闭宿主；超时或失败返回 unconfirmed，不抛出；重复调用返回同一结果。 */
  close(): Promise<PeriWasmHostCloseReport> {
    this.closePromise ??= this.closeOnce();
    return this.closePromise;
  }

  private async closeOnce(): Promise<PeriWasmHostCloseReport> {
    this.stateValue = "closing";
    try {
      await withDeadline(this.transportValue.close(), this.closeTimeoutMs, "Peri WASM host close");
      this.stateValue = "closed";
      return { confirmed: true, generationId: this.generationId };
    } catch (error) {
      this.stateValue = "close-unconfirmed";
      return { confirmed: false, error, generationId: this.generationId };
    }
  }
}

export interface PeriWasmHostManagerOptions {
  /** 启动超时；透传给每个租约。 */
  startupTimeoutMs?: number;
  /** 关闭超时；透传给每个租约。 */
  closeTimeoutMs?: number;
}

/**
 * 宿主租约登记表：统一启动入口、跟踪存活宿主，并在退出时收敛关闭证据。
 * 启动中的宿主也会被登记，`closeAll` 会先中止它们再等待结算。
 */
export class PeriWasmHostManager {
  /** 已成功启动、尚未确认关闭的租约。 */
  private readonly leases = new Set<PeriWasmHostLease>();
  /** 启动中的任务；`closeAll` 通过中止它们收敛并发启动。 */
  private readonly startups = new Map<AbortController, Promise<PeriWasmHostLease>>();

  constructor(private readonly starter: PeriWasmHostStarter,
    private readonly options: PeriWasmHostManagerOptions = {}) {}

  /** 启动并登记一个宿主；外部信号中止时启动一并取消，返回的租约由调用方负责 `close`。 */
  async acquire(options: { signal?: AbortSignal } = {}): Promise<PeriWasmHostLease> {
    const controller = new AbortController();
    const abort = (): void => controller.abort(options.signal?.reason);
    if (options.signal?.aborted) abort();
    else options.signal?.addEventListener("abort", abort, { once: true });
    const task = PeriWasmHostLease.start(this.starter, {
      startupTimeoutMs: this.options.startupTimeoutMs,
      closeTimeoutMs: this.options.closeTimeoutMs,
      signal: controller.signal,
    });
    this.startups.set(controller, task);
    try {
      const lease = await task;
      this.leases.add(lease);
      return lease;
    } finally {
      this.startups.delete(controller);
      options.signal?.removeEventListener("abort", abort);
    }
  }

  /** 当前跟踪的宿主数（含关闭未确认的宿主）。 */
  get size(): number { return this.leases.size; }

  /** 是否存在关闭未确认的宿主；为真时调用方不得再启动新执行。 */
  get unconfirmed(): boolean {
    return [...this.leases].some((lease) => lease.state === "close-unconfirmed");
  }

  /** 中止启动中的宿主，关闭其余宿主；返回每个宿主的关闭证据。 */
  async closeAll(): Promise<PeriWasmHostCloseReport[]> {
    const startups = [...this.startups];
    for (const [controller] of startups) controller.abort(new Error("Peri WASM host manager shutdown"));
    await Promise.allSettled(startups.map(([, task]) => task));
    const reports = await Promise.all([...this.leases].map((lease) => lease.close()));
    for (const lease of this.leases) if (lease.state === "closed") this.leases.delete(lease);
    return reports;
  }

  /** 关闭单个宿主；确认后不再跟踪，未确认时保留以便诊断。 */
  async release(lease: PeriWasmHostLease): Promise<PeriWasmHostCloseReport> {
    const report = await lease.close();
    if (report.confirmed) this.leases.delete(lease);
    return report;
  }
}
