export class DemoSessionNotFoundError extends Error {
  constructor() {
    super("Session does not belong to this Sandbox");
    this.name = "DemoSessionNotFoundError";
  }
}
