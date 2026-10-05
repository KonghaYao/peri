import type { CommandExpectation, ControlCommand, ControlState } from "../src/agent/session-control";
import type { Session } from "../src/agent/session";

export const closeCommand: CommandExpectation = {
  commandId: "close-session", expectedLifecycle: 1,
  expectedRevision: 0, expectedControlGeneration: 0,
};

const commands = new WeakMap<Pick<Session, "controlState">, CommandExpectation>();

export async function closeExpectation(session: Pick<Session, "controlState">): Promise<CommandExpectation> {
  const previous = commands.get(session);
  if (previous) return previous;
  const { state } = await session.controlState();
  const command = {
    commandId: crypto.randomUUID(), expectedLifecycle: state.lifecycle,
    expectedRevision: state.revision, expectedControlGeneration: state.controlGeneration,
  };
  commands.set(session, command);
  return command;
}

export function controlResponse(method: string, params?: unknown): unknown {
  const state: ControlState = {
    lifecycle: 1, revision: 1, controlGeneration: 1,
    status: "closed", attempt: null,
  };
  if (method === "session/control/state")
    return { state, settlement: { status: "settled" } };
  const command = params as ControlCommand;
  if (method === "session/control")
    return {
      sessionId: command.sessionId, commandId: command.commandId,
      decision: { kind: "accepted" }, state: { ...state, status: "closing" },
    };
  throw new Error(`Unexpected control fixture method: ${method}`);
}
