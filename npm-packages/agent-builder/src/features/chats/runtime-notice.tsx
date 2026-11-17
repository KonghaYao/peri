import type { EngineHealth } from '@shared/api';
import { isRetryableRuntime, type SandboxStatus } from '@shared/chat';
import { AlertTriangle } from 'lucide-react';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { ApiClientError } from '@/lib/api-client';

export interface ChatRuntimeNoticeProps {
  readonly mode: 'wasm' | 'not_configured';
  readonly reasons: readonly string[];
}

const REASON_HINTS: Record<string, string> = {
  store_token_missing: '缺少 Store 认证令牌（访问会话存储用，与模型 API key 不同）：设置 PERI_STORE_TOKEN',
  store_unreachable:
    '无法连接 Peri Store（网络错误、超时或 5xx）：检查 PERI_STORE_URL 与网络；Store 恢复后会自动重试',
  store_auth_failed:
    'Store 认证失败（401/403）：检查 PERI_STORE_TOKEN 是否有效；修正后需重启本地 API，不会自动重试',
  store_database_missing:
    'Store 数据库不存在（404）：检查 PERI_STORE_URL 中的数据库与路径；修正后需重启本地 API，不会自动重试',
  provider_key_missing: '缺少模型 API key：请在 Providers 管理页填写 SQLite 凭据',
  wasm_assets_missing: 'SDK 缺少 wasm 运行时资源（@peri-code/sdk/dist/wasm）',
  rivet_storage_path_missing: '缺少本地引擎数据目录：设置 RIVETKIT_STORAGE_PATH（默认 ./.data/rivet）',
};

/** Compact labels for surfaces that cannot fit REASON_HINTS, such as the sidebar. */
const RUNTIME_REASON_LABELS: Record<string, string> = {
  store_token_missing: '缺少 Store 认证令牌',
  store_unreachable: '无法连接 Store',
  store_auth_failed: 'Store 认证失败',
  store_database_missing: 'Store 数据库不存在',
  provider_key_missing: '缺少模型 API key',
  wasm_assets_missing: '缺少 wasm 运行时资源',
  rivet_storage_path_missing: '缺少本地引擎数据目录',
};

/** Store outages are operational failures, not missing deployment configuration. */
const STORE_FAILURE_REASONS = new Set(['store_unreachable', 'store_auth_failed', 'store_database_missing']);

/** Reasons that mean a deployment setting is absent; anything else keeps the state unknown. */
const CONFIGURATION_REASONS = new Set([
  'store_token_missing',
  'provider_key_missing',
  'wasm_assets_missing',
  'rivet_storage_path_missing',
]);

export interface RuntimeSummary {
  readonly label: string;
  /** Short reason line; null when the state already explains itself. */
  readonly detail: string | null;
}

/** One mapping for the alert title and the sidebar, so both surfaces agree on the cause. */
export function runtimeSummary(status: EngineHealth['status'], reasons: readonly string[]): RuntimeSummary {
  if (status === 'ready') return { label: '对话就绪', detail: null };
  if (status === 'starting') return { label: '对话启动中', detail: null };
  if (status === 'unavailable') return { label: '对话启动失败', detail: null };
  if (reasons.length === 0) return { label: '对话状态未知', detail: null };

  const storeFailure = reasons.some((reason) => STORE_FAILURE_REASONS.has(reason));
  const configurationMissing = reasons.every((reason) => CONFIGURATION_REASONS.has(reason));
  return {
    label: storeFailure ? '对话暂不可用' : configurationMissing ? '对话未配置' : '对话状态未知',
    detail: reasons.map((reason) => RUNTIME_REASON_LABELS[reason] ?? reason).join('；'),
  };
}

const SANDBOX_REASON_HINTS: Record<string, string> = {
  sandbox_peri_missing: '找不到 Peri 可执行文件：设置 PERI_SANDBOX_PERI_PATH',
  sandbox_settings_missing: '找不到 Sandbox settings 文件：检查 PERI_SANDBOX_SETTINGS',
  sandbox_settings_not_private: 'Sandbox settings 必须是 0600 的私有文件',
  sandbox_workspace_not_private: 'PERI_WORKSPACE_PATH 必须是已存在的 0700 目录',
  sandbox_workspace_non_canonical: 'PERI_WORKSPACE_PATH 必须是规范真实路径（不能含 symlink）',
  sandbox_root_not_private: 'Sandbox 状态目录必须是 0700',
  sandbox_root_overlaps_workspace: 'Sandbox 状态目录不能与工作区互相包含',
  sandbox_token_missing: 'Sandbox 凭据文件不可用；检查数据目录权限',
  sandbox_mcp_disabled: '该 Agent 显式关闭了 McpMiddleware；Sandbox 需要 MCP 能力',
};

/** Deployment-level sandbox readiness; per-chat state is shown on the chat page. */
export function SandboxStatusNotice({ status }: { readonly status: SandboxStatus }) {
  if (status.mode !== 'misconfigured') return null;
  return (
    <Alert variant="destructive">
      <AlertTriangle />
      <AlertTitle>Sandbox 已启用但配置不完整</AlertTitle>
      <AlertDescription>
        <p>创建对话会被拒绝，直到部署配置修正并重启：</p>
        <ul className="mt-2 list-disc space-y-1 pl-5">
          {status.reasons.map((reason) => (
            <li key={reason}>{SANDBOX_REASON_HINTS[reason] ?? reason}</li>
          ))}
        </ul>
      </AlertDescription>
    </Alert>
  );
}

/** Reads the reasons the API attached to a 503 instead of guessing from the message. */
export function runtimeReasonsFromError(error: unknown): string[] {
  if (!(error instanceof ApiClientError) || error.code !== 'engine_unavailable') return [];
  const raw = error.details.reasons;
  if (!Array.isArray(raw)) return [];
  return raw.filter((value): value is string => typeof value === 'string');
}

/**
 * Store 暂时不可达时可重试；基础设施凭据或数据库配置修正后需要重启。
 */
export { isRetryableRuntime };

export function ChatRuntimeNotice({ mode, reasons }: ChatRuntimeNoticeProps) {
  if (mode === 'wasm') return null;
  const summary = runtimeSummary('unknown', reasons);

  return (
    <Alert>
      <AlertTriangle />
      <AlertTitle>{summary.label}</AlertTitle>
      <AlertDescription>
        {isRetryableRuntime(reasons) ? (
          <p>网络问题会在 Store 恢复后自动重试，无需重启本地 API：</p>
        ) : (
          <p>以下问题需要修正后重启本地 API：</p>
        )}
        <ul className="mt-2 list-disc space-y-1 pl-5">
          {reasons.length === 0 ? <li>未提供具体原因，请查看 API 日志。</li> : null}
          {reasons.map((reason) => (
            <li key={reason}>{REASON_HINTS[reason] ?? reason}</li>
          ))}
        </ul>
      </AlertDescription>
    </Alert>
  );
}
