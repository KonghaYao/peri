import type { WasmAcpTransport } from "./transport";
import { withDeadline } from "../util/deadline";
import {
  DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS, DEFAULT_WASM_RUNTIME_STARTUP_TIMEOUT_MS,
} from "./budget";
import { WasmRuntimeStartupError } from "./runtime";
import type { WasmRuntimeCleanupOutcome, WasmRuntimeDiagnosticKind } from "./runtime";

/** 单次运行后端启动的 per-chat 输入；全部可选以兼容旧的 starter。 */
export interface WasmRuntimeAcquireOptions {
  /** 外部取消信号；释放实例或进程退出时中止启动。 */
  signal?: AbortSignal;
  /** 该 Chat 的 WASM 启动配置；未提供时由默认 starter 使用 Manager 快照。 */
  configJson?: string;
  /** 该 Chat 合并后的 WASM env。 */
  env?: Readonly<Record<string, string>>;
  /** 该 Chat 的 WASM 诊断回调。 */
  onDiagnostic?: (kind: WasmRuntimeDiagnosticKind, error: unknown) => void;
}

export interface WasmRuntimeStarterOptions extends WasmRuntimeAcquireOptions {
  signal: AbortSignal;
}

/** 运行后端启动器：只依赖取消信号与 per-chat 启动配置，便于替换模块工厂或测试桩。 */
export type WasmRuntimeStarter = (
  options: WasmRuntimeStarterOptions,
) => Promise<WasmAcpTransport>;

/** 单运行后端状态：`live → closing → closed | close-unconfirmed`。 */
export type WasmRuntimeState = "live" | "closing" | "closed" | "close-unconfirmed";

export interface WasmRuntimeLeaseOptions {
  /** 启动超时；超时后中止启动并拒绝 `start`。 */
  startupTimeoutMs?: number;
  /** 关闭超时；超时或失败记为 `close-unconfirmed`。 */
  closeTimeoutMs?: number;
  /** 外部取消信号；释放实例或进程退出时中止启动。 */
  signal?: AbortSignal;
  /** 启动已判定失败、但迟到运行后端仍被关闭时的证据回调。 */
  onLateCleanup?: (outcome: WasmRuntimeCleanupOutcome) => void;
  /**
   * 启动失败后仍需结算的迟到清理。
   *
   * 清理可能在 `start` 拒绝之后才完成；池用它登记尚未结算的所有权，
   * 并在关闭时等待，避免「启动失败即无主」。
   */
  onLateCleanupStart?: (cleanup: Promise<WasmRuntimeCleanupOutcome>) => void;
}

export interface WasmRuntimeCloseReport extends WasmRuntimeCleanupOutcome {
  /** 运行后端身份；启动成功后固定。 */
  generationId: string;
}

/**
 * 单个 WASM 运行后端的所有权租约。
 * - `start` 统一启动、超时与取消，并负责关闭迟到运行后端；
 * - `close` 幂等且不抛出，返回 `confirmed` 证据；
 * - 调用方只依据 `state`/`confirmed` 决定是否继续使用运行后端。
 */
export class WasmRuntimeLease {
  /** 当前状态；`close-unconfirmed` 表示关闭超时或失败。 */
  private stateValue: WasmRuntimeState = "live";
  /** 关闭结算的幂等缓存。 */
  private closePromise: Promise<WasmRuntimeCloseReport> | undefined;

  /** 关闭超时；超时或失败记为 `close-unconfirmed`。 */
  private constructor(private readonly transportValue: WasmAcpTransport,
    private readonly closeTimeoutMs: number) {}

  /** 启动运行后端；超时/取消后中止内部信号并保留原始错误。 */
  static async start(starter: WasmRuntimeStarter,
    options: WasmRuntimeLeaseOptions = {}): Promise<WasmRuntimeLease> {
    const controller = new AbortController();
    const abort = (): void => controller.abort(options.signal?.reason);
    if (options.signal?.aborted) abort();
    else options.signal?.addEventListener("abort", abort, { once: true });
    const closeTimeoutMs = options.closeTimeoutMs ?? DEFAULT_WASM_RUNTIME_CLEANUP_TIMEOUT_MS;
    const starting = Promise.resolve().then(() => starter({ signal: controller.signal }));
    try {
      const transport = await withDeadline(
        starting,
        options.startupTimeoutMs ?? DEFAULT_WASM_RUNTIME_STARTUP_TIMEOUT_MS,
        "Peri WASM runtime startup",
      );
      return new WasmRuntimeLease(transport, closeTimeoutMs);
    } catch (error) {
      controller.abort(error);
      // 启动可能仍会成功，也可能迟到地以启动错误拒绝。两种情况都必须结算
      // 迟到运行后端的所有权，否则清理证据无人观察、容量会被错误复用。
      const settling = starting.then(
        async (late): Promise<WasmRuntimeCleanupOutcome | undefined> => {
          try {
            await withDeadline(late.close(), closeTimeoutMs, "Peri WASM late runtime close");
            return { confirmed: true };
          } catch (cleanupError) {
            return { confirmed: false, error: cleanupError };
          }
        },
        async (lateError): Promise<WasmRuntimeCleanupOutcome | undefined> => {
          if (!(lateError instanceof WasmRuntimeStartupError)) return undefined;
          try {
            return await lateError.cleanup;
          } catch (cleanupError) {
            return { confirmed: false, error: cleanupError };
          }
        },
      );
      options.onLateCleanupStart?.(settling);
      void settling.then((outcome) => {
        if (outcome !== undefined) options.onLateCleanup?.(outcome);
      });
      throw error;
    } finally {
      options.signal?.removeEventListener("abort", abort);
    }
  }

  /** 当前状态；关闭结果未确认时保持 `close-unconfirmed`。 */
  get state(): WasmRuntimeState { return this.stateValue; }
  /** 已启动宿主；仅在 `live`/`closing` 期间可发 ACP 请求。 */
  get transport(): WasmAcpTransport { return this.transportValue; }
  /** 宿主身份；日志与诊断使用。 */
  get generationId(): string { return this.transportValue.generationId; }

  /** 关闭宿主；超时或失败返回 unconfirmed，不抛出；重复调用返回同一结果。 */
  close(): Promise<WasmRuntimeCloseReport> {
    this.closePromise ??= this.closeOnce();
    return this.closePromise;
  }

  private async closeOnce(): Promise<WasmRuntimeCloseReport> {
    this.stateValue = "closing";
    try {
      await withDeadline(this.transportValue.close(), this.closeTimeoutMs, "Peri WASM runtime close");
      this.stateValue = "closed";
      return { confirmed: true, generationId: this.generationId };
    } catch (error) {
      this.stateValue = "close-unconfirmed";
      return { confirmed: false, error, generationId: this.generationId };
    }
  }
}

export interface WasmRuntimePoolOptions {
  /** 启动超时；透传给每个租约。 */
  startupTimeoutMs?: number;
  /** 关闭超时；透传给每个租约。 */
  closeTimeoutMs?: number;
}

/**
 * 运行后端租约登记表：统一启动入口、跟踪存活运行后端，并在退出时收敛关闭证据。
 * 启动中的运行后端也会被登记，`closeAll` 会先中止它们再等待结算。
 */
export class WasmRuntimePool {
  /** 已成功启动、尚未确认关闭的租约。 */
  private readonly leases = new Set<WasmRuntimeLease>();
  /** 启动中的任务；`closeAll` 通过中止它们收敛并发启动。 */
  private readonly startups = new Map<AbortController, Promise<WasmRuntimeLease>>();
  /** 启动失败后仍在结算的迟到清理；`closeAll` 会等待它们。 */
  private readonly pendingLateCleanups = new Set<Promise<WasmRuntimeCleanupOutcome>>();
  /** 任一次迟到清理未确认；在进程关闭前保持，避免继续复用容量。 */
  private lateCleanupUnconfirmed = false;

  constructor(private readonly starter: WasmRuntimeStarter,
    private readonly options: WasmRuntimePoolOptions = {}) {}

  /** 启动并登记一个运行后端；外部信号中止时启动一并取消，返回的租约由调用方负责 `close`。 */
  async acquire(options: WasmRuntimeAcquireOptions = {}): Promise<WasmRuntimeLease> {
    const controller = new AbortController();
    const abort = (): void => controller.abort(options.signal?.reason);
    if (options.signal?.aborted) abort();
    else options.signal?.addEventListener("abort", abort, { once: true });
    const runtime = {
      ...(options.configJson === undefined ? {} : { configJson: options.configJson }),
      ...(options.env === undefined ? {} : { env: options.env }),
      ...(options.onDiagnostic === undefined ? {} : { onDiagnostic: options.onDiagnostic }),
    };
    const task = WasmRuntimeLease.start(
      ({ signal }) => this.starter({ ...runtime, signal }),
      {
        startupTimeoutMs: this.options.startupTimeoutMs,
        closeTimeoutMs: this.options.closeTimeoutMs,
        signal: controller.signal,
        onLateCleanupStart: (cleanup) => {
          this.pendingLateCleanups.add(cleanup);
          void cleanup.finally(() => this.pendingLateCleanups.delete(cleanup));
        },
        onLateCleanup: (outcome) => {
          if (!outcome.confirmed) this.lateCleanupUnconfirmed = true;
        },
      },
    );
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

  /** 当前跟踪的运行后端数（含关闭未确认的运行后端）。 */
  get size(): number { return this.leases.size; }

  /** 是否存在关闭未确认的运行后端；为真时调用方不得再启动新执行。 */
  get unconfirmed(): boolean {
    return this.lateCleanupUnconfirmed
      || [...this.leases].some((lease) => lease.state === "close-unconfirmed");
  }

  /** 中止启动中的运行后端，关闭其余运行后端；返回每个运行后端的关闭证据。 */
  async closeAll(): Promise<WasmRuntimeCloseReport[]> {
    const startups = [...this.startups];
    for (const [controller] of startups) controller.abort(new Error("Peri WASM runtime pool shutdown"));
    await Promise.allSettled(startups.map(([, task]) => task));
    // 启动失败但迟到清理尚未结算的运行后端不在 `leases` 里，必须单独等待；
    // 若启动器不响应中止、清理永不结算，等待必须有界，否则关闭会永久挂起。
    if (this.pendingLateCleanups.size > 0) {
      try {
        await withDeadline(
          Promise.allSettled([...this.pendingLateCleanups]),
          this.options.closeTimeoutMs,
          "Peri WASM late runtime cleanup",
        );
      } catch {
        this.lateCleanupUnconfirmed = true;
      }
    }
    const reports = await Promise.all([...this.leases].map((lease) => lease.close()));
    for (const lease of this.leases) if (lease.state === "closed") this.leases.delete(lease);
    return reports;
  }

  /** 关闭单个运行后端；确认后不再跟踪，未确认时保留以便诊断。 */
  async release(lease: WasmRuntimeLease): Promise<WasmRuntimeCloseReport> {
    const report = await lease.close();
    if (report.confirmed) this.leases.delete(lease);
    return report;
  }
}
