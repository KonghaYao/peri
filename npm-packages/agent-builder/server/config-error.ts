import type { SandboxReason } from '@shared/chat';

/**
 * 部署配置错误的稳定诊断码与中文提示。
 *
 * 只暴露白名单 code 与静态提示：原始异常留在 cause 内，不回显 Error message、路径或环境值。
 * 这些 code 不进入 API 错误码集合，避免扩大 HTTP 错误体可接受的诊断字段。
 */

/** 启动阶段可产生的 sandbox 部署码；拼写与 `shared/chat.ts` 的公共 reason 保持一致。 */
export const SANDBOX_CONFIG_ERROR_CODES = [
  'sandbox_peri_missing',
  'sandbox_settings_missing',
  'sandbox_settings_not_private',
  'sandbox_workspace_not_private',
  'sandbox_root_not_private',
  'sandbox_root_overlaps_workspace',
  'sandbox_token_missing',
] as const satisfies readonly SandboxReason[];

export type SandboxConfigErrorCode = (typeof SANDBOX_CONFIG_ERROR_CODES)[number];

const SANDBOX_CONFIG_ERROR_CODE_SET = new Set<string>(SANDBOX_CONFIG_ERROR_CODES);

export const CONFIG_ERROR_CODES = [
  'config_invalid',
  'config_machine_id_invalid',
  'workspace_path_missing',
  'workspace_path_symlink',
  'workspace_path_not_directory',
  'workspace_path_not_canonical',
  'workspace_path_filesystem_root',
  ...SANDBOX_CONFIG_ERROR_CODES,
] as const;

export type ConfigErrorCode = (typeof CONFIG_ERROR_CODES)[number];

const CONFIG_ERROR_HINTS: Record<ConfigErrorCode, string> = {
  config_invalid: '环境变量格式无效；请按 .env.example 检查字段格式',
  config_machine_id_invalid: 'PERI_MACHINE_ID 必须是 UUID；留空则使用数据目录内的 machine-id',
  workspace_path_missing: 'PERI_WORKSPACE_PATH 不存在；请先创建目录，再填写 realpath 后的绝对路径',
  workspace_path_symlink: 'PERI_WORKSPACE_PATH 不能是符号链接；请填写该目录 realpath 后的真实路径',
  workspace_path_not_directory: 'PERI_WORKSPACE_PATH 必须是目录；请检查配置',
  // macOS 的 /tmp 是 /private/tmp 的符号链接，这是最常见的非规范路径来源。
  workspace_path_not_canonical:
    'PERI_WORKSPACE_PATH 含符号链接目录组件；请用 realpath 后的真实路径（macOS 的 /tmp 实际是 /private/tmp）',
  workspace_path_filesystem_root: 'PERI_WORKSPACE_PATH 不能是文件系统根目录；请指定专用 workspace 目录',
  sandbox_peri_missing:
    '缺少 Peri 可执行文件；请设置 PERI_SANDBOX_PERI_PATH（默认 ~/.peri/peri）指向已安装的 peri',
  sandbox_settings_missing: 'PERI_SANDBOX_SETTINGS 指向的文件不存在；请修正路径，或留空由应用生成',
  sandbox_settings_not_private: 'PERI_SANDBOX_SETTINGS 必须是 0600 的普通文件；请修正文件权限',
  sandbox_workspace_not_private:
    'PERI_WORKSPACE_PATH 必须是 0700 的真实目录；请修正目录权限（Sandbox 不会自动 chmod）',
  sandbox_root_not_private: 'PERI_SANDBOX_ROOT 必须是 0700 的真实目录；请修正权限，或删除后由应用重新创建',
  sandbox_root_overlaps_workspace: 'PERI_SANDBOX_ROOT 不能与 PERI_WORKSPACE_PATH 互相包含；请改用独立目录',
  sandbox_token_missing:
    'PERI_SANDBOX_TOKEN 无效；请留空由应用生成（0600），或提供 16-512 字符且不含空白的值',
};

/** 最多展示 5 个由 schema 白名单过滤出的字段名，避免提示过长。 */
const MAX_HINT_FIELDS = 5;

export interface ConfigErrorOptions {
  readonly cause?: unknown;
  /** 仅允许 EnvSchema 的字段名；用于 `config_invalid` 的可行动提示。 */
  readonly fields?: readonly string[];
}

export class ConfigError extends Error {
  readonly code: ConfigErrorCode;
  readonly fields: readonly string[];

  constructor(code: ConfigErrorCode, options: ConfigErrorOptions = {}) {
    // message 固定为 code，避免把原始异常文本带出边界。
    super(code, { cause: options.cause });
    this.name = 'ConfigError';
    this.code = code;
    this.fields = options.fields ?? [];
  }
}

export interface ConfigFailure {
  readonly code: ConfigErrorCode;
  readonly hint: string;
}

/** 仅识别 ConfigError；其他异常继续走调用方的 internal_error 安全兜底。 */
export function configFailure(error: unknown): ConfigFailure | null {
  if (!(error instanceof ConfigError)) return null;
  if (error.code === 'config_invalid' && error.fields.length > 0) {
    const shown = error.fields.slice(0, MAX_HINT_FIELDS).join('、');
    const suffix = error.fields.length > MAX_HINT_FIELDS ? ' 等' : '';
    return { code: error.code, hint: `环境变量格式无效；请按 .env.example 检查 ${shown}${suffix}` };
  }
  return { code: error.code, hint: CONFIG_ERROR_HINTS[error.code] };
}

/** 只把白名单内的 sandbox 启动原因转成静态 ConfigError；未知原因交给 internal_error 兜底。 */
export function sandboxStartupError(reason: SandboxReason): ConfigError | null {
  if (!SANDBOX_CONFIG_ERROR_CODE_SET.has(reason)) return null;
  return new ConfigError(reason as SandboxConfigErrorCode);
}
