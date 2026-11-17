import { PeriError } from './errors.js';

declare const contractIdentityBrand: unique symbol;

/** 包内共享 brand；只通过各契约模块间接导出，不进入 `/contracts` 运行时。 */
export type Brand<T, B extends string> = T & { readonly [contractIdentityBrand]: B };

export type SessionId = Brand<string, 'SessionId'>;

/**
 * chat actor 的路由键与所有权键，由客户端在创建 chat 时生成。
 *
 * 与 `SessionId` 分开：actor 存活期间 ACP Session 可能经 `change_session` 切换，
 * 而 chatId 在整个 actor 生命周期里不变。
 */
export type ChatId = Brand<string, 'ChatId'>;

/**
 * Rust 事实身份，只随事件信封出现；当前恒为 1 且未上 wire，客户端不得据此推断实例重建。
 */
export type SessionEpoch = Brand<number, 'SessionEpoch'>;

export type TurnId = Brand<string, 'TurnId'>;

/**
 * 每次尝试的 id。ACP forwarder 目前不转发 attempt，`eventIdentity` 落地前 wire 值恒为 null；
 * 类型显式包含 null，避免调用方把“暂时缺失”当成错误。
 */
export type AttemptId = Brand<string, 'AttemptId'> | null;

/** 不透明配置版本；只用于比较是否变化，不承诺单调递增。 */
export type ConfigRevision = Brand<string, 'ConfigRevision'>;

const IDENTITY_WHITESPACE = /\s/u;
const DEFAULT_MAX_LENGTH = 200;

/**
 * 身份校验失败一律用 `invalid_identity`，与普通参数错误的 `invalid_input` 分开：
 * 调用方需要知道“这个 id 不能用于寻址/存储”，而不是笼统的输入不合法。
 */
function invalidIdentity(label: string, reason: string): never {
  throw new PeriError({
    code: 'invalid_identity',
    phase: 'input',
    message: `${label} ${reason}`,
    details: { label, reason },
  });
}

function hasForbiddenCharacter(value: string): boolean {
  for (const character of value) {
    const codePoint = character.codePointAt(0);
    if (codePoint === undefined) {
      continue;
    }
    if (codePoint <= 0x1f || codePoint === 0x7f || IDENTITY_WHITESPACE.test(character)) {
      return true;
    }
  }
  return false;
}

/** 包内共享：服务端身份复用同一套校验，避免两套规则漂移。 */
export function parseIdentityText<B extends string>(
  label: string,
  value: string,
  maxLength = DEFAULT_MAX_LENGTH,
): Brand<string, B> {
  if (typeof value !== 'string') {
    invalidIdentity(label, 'must be a string');
  }
  if (value.length === 0 || value.length > maxLength) {
    invalidIdentity(label, `must have 1..${maxLength} characters`);
  }
  if (hasForbiddenCharacter(value)) {
    invalidIdentity(label, 'must not contain whitespace or control characters');
  }
  return value as Brand<string, B>;
}

/** epoch 从 1 开始；0 或小数没有合法来源，拒绝比事后归一化更安全。 */
export function parseIdentityEpoch<B extends string>(label: string, value: number): Brand<number, B> {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 1) {
    invalidIdentity(label, 'must be an integer >= 1');
  }
  return value as Brand<number, B>;
}

export function parseSessionId(value: string): SessionId {
  return parseIdentityText('sessionId', value);
}

export function parseChatId(value: string): ChatId {
  return parseIdentityText('chatId', value);
}

export function parseSessionEpoch(value: number): SessionEpoch {
  return parseIdentityEpoch('sessionEpoch', value);
}

export function parseTurnId(value: string): TurnId {
  return parseIdentityText('turnId', value);
}

export function parseAttemptId(value: string | null): AttemptId {
  return value === null ? null : parseIdentityText('attemptId', value);
}

export function parseConfigRevision(value: string): ConfigRevision {
  return parseIdentityText('configRevision', value);
}
