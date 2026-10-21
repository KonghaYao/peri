/** 第一版对外可见的业务错误码（§8.3）。 */
export type BusinessErrorCode =
  | 'NOT_MEMBER'
  | 'NOT_OWNER'
  | 'QUEUE_FULL'
  | 'PAYLOAD_TOO_LARGE'
  | 'CLAIM_CONFLICT'
  | 'STALE_ATTEMPT'
  | 'IDEMPOTENCY_CONFLICT'
  | 'CURSOR_EXPIRED'
  | 'RATE_LIMITED'
  | 'NOT_FOUND'
  | 'INVALID_ARGUMENT'
  | 'ACTOR_EXISTS'
  | 'CREDENTIAL_INVALID'
  | 'RECOVERY_INVALID'
  | 'ROOM_FULL'
  | 'FRIEND_REQUIRED'
  | 'CONFLICT';

export interface BusinessErrorBody {
  code: BusinessErrorCode;
  message: string;
  retryable: boolean;
  details?: Record<string, unknown>;
}

const RETRYABLE: ReadonlySet<BusinessErrorCode> = new Set(['RATE_LIMITED', 'CONFLICT']);

export class BusinessError extends Error {
  readonly code: BusinessErrorCode;
  readonly retryable: boolean;
  readonly details: Record<string, unknown> | undefined;

  constructor(code: BusinessErrorCode, message: string, details?: Record<string, unknown>) {
    super(message);
    this.name = 'BusinessError';
    this.code = code;
    this.retryable = RETRYABLE.has(code);
    this.details = details;
  }

  toBody(): BusinessErrorBody {
    return {
      code: this.code,
      message: this.message,
      retryable: this.retryable,
      ...(this.details ? { details: this.details } : {}),
    };
  }
}

export function isBusinessError(error: unknown): error is BusinessError {
  return error instanceof BusinessError;
}
