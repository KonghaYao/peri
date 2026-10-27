import {
  SandboxManagerError,
  type SandboxBackend,
  type SandboxBackendInstance,
  type SandboxBackendStartOptions,
  type SandboxInstanceState,
} from "./types";

export const DEFAULT_SANDBOX_MODULE = "peri-sandbox";

interface PeriSandboxModule {
  startSandbox(options: SandboxBackendStartOptions): Promise<unknown>;
  deleteSandboxData(options: { sandboxId: string; sandboxesRoot?: string; settingsPath?: string }): Promise<void>;
}

const states = new Set<SandboxInstanceState>(["starting", "ready", "stopping", "stopped", "failed", "closed"]);

function record(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : null;
}

function requiredString(source: Record<string, unknown>, field: string): string {
  const value = source[field];
  if (typeof value !== "string" || value.length === 0)
    throw new SandboxManagerError(
      "backend_unavailable",
      `The "${DEFAULT_SANDBOX_MODULE}" module returned an invalid sandbox instance ("${field}").`,
    );
  return value;
}

function mapInstance(value: unknown): SandboxBackendInstance {
  const instance = record(value);
  if (!instance || typeof instance.close !== "function")
    throw new SandboxManagerError(
      "backend_unavailable",
      `The "${DEFAULT_SANDBOX_MODULE}" module returned an invalid sandbox instance.`,
    );
  const close = instance.close as (options?: { deleteData?: boolean }) => Promise<void>;
  const tokenFile = instance.tokenFile;
  return {
    id: requiredString(instance, "id"),
    runId: requiredString(instance, "runId"),
    mcpUrl: requiredString(instance, "mcpUrl"),
    token: requiredString(instance, "token"),
    workspacePath: requiredString(instance, "workspacePath"),
    sandboxRoot: requiredString(instance, "sandboxRoot"),
    tokenFile: typeof tokenFile === "string" ? tokenFile : null,
    get state(): SandboxInstanceState {
      const state = instance.state;
      return typeof state === "string" && states.has(state as SandboxInstanceState)
        ? (state as SandboxInstanceState)
        : "failed";
    },
    close: (options) => close.call(instance, options),
  };
}

function assertModule(value: unknown): PeriSandboxModule {
  const module = record(value);
  if (!module || typeof module.startSandbox !== "function" || typeof module.deleteSandboxData !== "function")
    throw new SandboxManagerError(
      "backend_unavailable",
      `The "${DEFAULT_SANDBOX_MODULE}" module does not export the single-instance sandbox API.`,
    );
  return module as unknown as PeriSandboxModule;
}

/**
 * Build a lazy backend around the optional `peri-sandbox` package.
 *
 * The module specifier is a runtime variable so bundlers keep the import
 * dynamic; the package is never loaded by non-sandbox entries.
 */
export function loadSandboxBackend(moduleSpecifier: string = DEFAULT_SANDBOX_MODULE): SandboxBackend {
  let loading: Promise<PeriSandboxModule> | null = null;

  const load = (): Promise<PeriSandboxModule> => {
    if (loading) return loading;
    const specifier: string = moduleSpecifier;
    const attempt = import(specifier).then(assertModule).catch((error: unknown) => {
      if (error instanceof SandboxManagerError) throw error;
      throw new SandboxManagerError(
        "backend_unavailable",
        `Cannot load "${specifier}". Install peri-sandbox (>=0.1.0) to use the sandbox entry.`,
        { cause: error },
      );
    });
    loading = attempt;
    attempt.catch(() => {
      if (loading === attempt) loading = null;
    });
    return attempt;
  };

  return {
    async start(options) {
      const module = await load();
      return mapInstance(await module.startSandbox(options));
    },
    async deleteData(options) {
      const module = await load();
      await module.deleteSandboxData(options);
    },
  };
}
