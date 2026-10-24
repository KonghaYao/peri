/** 共享协调域中已有其他 owner 持有该 Session claim。 */
export class SessionClaimConflictError extends Error {
  constructor(
    readonly key: string,
    readonly sessionId?: string,
  ) {
    super(
      sessionId === undefined
        ? `Session is already claimed: ${key}`
        : `Session is already claimed by another owner: ${sessionId}`,
    );
    this.name = "SessionClaimConflictError";
  }
}
