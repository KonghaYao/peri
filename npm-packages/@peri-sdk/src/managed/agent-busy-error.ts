export class AgentBusyError extends Error {
  constructor(id: string) {
    super(`Agent is already declared in this manager: ${id}`);
    this.name = "AgentBusyError";
  }
}
