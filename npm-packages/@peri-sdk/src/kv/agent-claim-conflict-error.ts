export class AgentClaimConflictError extends Error {
  constructor(readonly key: string) {
    super(`Agent or session is already owned: ${key}`);
    this.name = "AgentClaimConflictError";
  }
}
