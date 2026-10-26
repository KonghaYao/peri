/** 共享协调域中已有其他 owner 持有该 Session 的执行权。 */
export class SessionOwnershipConflictError extends Error {
  constructor(
    readonly key: string,
    readonly sessionId?: string,
  ) {
    super(
      sessionId === undefined
        ? `Session is already owned: ${key}`
        : `Session is already owned by another instance: ${sessionId}`,
    );
    this.name = "SessionOwnershipConflictError";
  }
}
