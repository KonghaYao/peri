import { describe, expect, test } from "bun:test";
import { AgentClaimConflictError } from "../src/kv/agent-claim-conflict-error";
import { AgentClaims, agentClaimKey, sessionClaimKey } from "../src/kv/agent-claims";
import { MemoryKV } from "../src/kv/memory-kv";

describe("AgentClaims coordination domain", () => {
  test("different Sandboxes racing for the same Session have exactly one owner", async () => {
    const kv = new MemoryKV();
    const first = new AgentClaims(kv, "sandbox-1", "agent-1");
    const second = new AgentClaims(kv, "sandbox-2", "agent-1");
    await Promise.all([first.claimAgent(), second.claimAgent()]);

    const results = await Promise.allSettled([
      first.claimSession("session-1"),
      second.claimSession("session-1"),
    ]);

    expect(results.filter((result) => result.status === "fulfilled")).toHaveLength(1);
    const rejected = results.find((result) => result.status === "rejected") as PromiseRejectedResult;
    expect(rejected.reason).toBeInstanceOf(AgentClaimConflictError);
    expect(rejected.reason.key).toBe(sessionClaimKey("session-1"));
    await Promise.all([first.release(), second.release()]);
  });

  test("different Sessions in different Sandboxes do not conflict", async () => {
    const kv = new MemoryKV();
    const first = new AgentClaims(kv, "sandbox-1", "agent-1");
    const second = new AgentClaims(kv, "sandbox-2", "agent-1");
    await Promise.all([first.claimAgent(), second.claimAgent()]);

    await Promise.all([first.claimSession("session-1"), second.claimSession("session-2")]);

    await Promise.all([first.release(), second.release()]);
  });

  test("a Session can be taken over only after its original owner releases it", async () => {
    const kv = new MemoryKV();
    const original = new AgentClaims(kv, "sandbox-1", "agent-1");
    const contender = new AgentClaims(kv, "sandbox-2", "agent-1");
    await original.claimAgent();
    await original.claimSession("session-1");
    await contender.claimAgent();

    await expect(contender.claimSession("session-1")).rejects.toBeInstanceOf(AgentClaimConflictError);
    expect(await kv.releaseIfOwner(sessionClaimKey("session-1"), "another-owner")).toBe(false);
    await contender.release();
    await expect(contender.claimSession("session-1")).rejects.toBeInstanceOf(AgentClaimConflictError);

    await original.release();
    await contender.claimAgent();
    await contender.claimSession("session-1");
    await original.release();
    await expect(original.claimSession("session-1")).rejects.toBeInstanceOf(AgentClaimConflictError);
    await contender.release();
  });

  test("Agent claims remain scoped by Sandbox and Agent", async () => {
    const kv = new MemoryKV();
    const first = new AgentClaims(kv, "sandbox-1", "agent-1");
    const duplicate = new AgentClaims(kv, "sandbox-1", "agent-1");
    const otherSandbox = new AgentClaims(kv, "sandbox-2", "agent-1");
    const otherAgent = new AgentClaims(kv, "sandbox-1", "agent-2");

    await Promise.all([first.claimAgent(), otherSandbox.claimAgent(), otherAgent.claimAgent()]);
    await expect(duplicate.claimAgent()).rejects.toBeInstanceOf(AgentClaimConflictError);
    expect(agentClaimKey("sandbox:1", "agent:1")).toBe("peri:managed-agent:sandbox%3A1:agent:agent%3A1");
    expect(sessionClaimKey("session:1")).toBe("peri:managed-agent:session:session%3A1");
    await Promise.all([first.release(), otherSandbox.release(), otherAgent.release()]);
  });

  test("separate KV keyspaces coordinate independently", async () => {
    const first = new AgentClaims(new MemoryKV(), "sandbox-1", "agent-1");
    const second = new AgentClaims(new MemoryKV(), "sandbox-1", "agent-1");

    await Promise.all([first.claimAgent(), second.claimAgent()]);
    await Promise.all([first.claimSession("session-1"), second.claimSession("session-1")]);

    await Promise.all([first.release(), second.release()]);
  });
});
