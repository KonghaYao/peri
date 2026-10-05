import type { Agent, CommandExpectation } from "../../src/sdk/index";

export async function shutdownCommands(agents: Iterable<Agent>): Promise<Map<string, CommandExpectation>> {
    const commands = new Map<string, CommandExpectation>();
    for (const agent of agents) {
        if (commands.has(agent.id)) continue;
        const { state } = await agent.session.controlState();
        commands.set(agent.id, {
            commandId: crypto.randomUUID(),
            expectedLifecycle: state.lifecycle,
            expectedRevision: state.revision,
            expectedControlGeneration: state.controlGeneration,
        });
    }
    return commands;
}
