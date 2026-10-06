import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { DatabaseSync } from "node:sqlite";

interface Event {
  producerNamespace: string;
  eventId: string;
  eventKind: string;
  causationId: string;
  content: { serialized: string };
}

interface Receipt {
  sessionId: string;
  mutationId: string;
  deliveryId: string;
  decision: { kind: string };
}

interface State {
  deliveries: Record<string, {
    recipientLifecycle: number;
    batchId: string | null;
    publication: { purpose: string; policy: { requirement: string }; event: Event };
  }>;
  obligations: Record<string, { status: string; workId: string | null }>;
  batches: Record<string, { recipientLifecycle: number; processingDeliveryIds: string[] }>;
  works: Record<string, { batchId: string }>;
  invocations: Record<string, unknown>;
  taskBindings: Record<string, {
    invocationId: string; ownerTaskId: string; initiatorSessionId: string; recipientLifecycle: number;
  }>;
  terminalObligations: Record<string, {
    sessionId: string; mutationId: string; recipientLifecycle: number;
    action: {
      kind: string; delivery: { deliveryId: string };
      binding: State["taskBindings"][string];
    };
  }>;
  terminalAcknowledgements: Record<string, Receipt>;
  admissions: Record<string, {
    admission: { sessionId: string; workId: string };
    settledReceipt: Receipt | null;
  }>;
  workDelegations: Record<string, State["taskBindings"][string]>;
}

export interface BgStageBoundary {
  parentSessionId?: string;
  invocationIds: string[];
}

export interface BgCompletionEvidence {
  parentSessionId: string;
  invocationId: string;
  deliveryId: string;
  batchId: string;
  childSessionId?: string;
  childAdmissionId?: string;
}

function read<T>(home: string, inspect: (database: DatabaseSync) => T): T | undefined {
  const file = path.join(home, ".peri", "threads", "threads.db");
  if (!existsSync(file)) return undefined;
  const database = new DatabaseSync(file, { readOnly: true });
  try {
    database.exec("BEGIN");
    return inspect(database);
  } finally {
    database.close();
  }
}

function parent(database: DatabaseSync): { id: string; state: State } | undefined {
  const rows = database.prepare(
    "SELECT t.id, s.state_json FROM threads t JOIN session_work_state s ON s.session_id=t.id WHERE t.parent_thread_id IS NULL",
  ).all() as { id: string; state_json: string }[];
  if (rows.length === 0) return undefined;
  if (rows.length !== 1) throw new Error("BG fixture requires exactly one direct parent session");
  return { id: rows[0].id, state: JSON.parse(rows[0].state_json) as State };
}

export function captureBgStageBoundary(home: string): BgStageBoundary {
  return read(home, (database) => {
    const root = parent(database);
    return { parentSessionId: root?.id, invocationIds: Object.keys(root?.state.invocations ?? {}) };
  }) ?? { invocationIds: [] };
}

export function completedBgStage(
  home: string,
  boundary: BgStageBoundary,
  source: "shell" | "subagent",
  marker: string,
): BgCompletionEvidence | undefined {
  return read(home, (database) => {
    const root = parent(database);
    if (!root || (boundary.parentSessionId && boundary.parentSessionId !== root.id)) return undefined;
    for (const [deliveryId, delivery] of Object.entries(root.state.deliveries)) {
      const event = delivery.publication.event;
      if (delivery.publication.purpose !== "taskTerminal"
        || delivery.publication.policy.requirement !== "required"
        || event.producerNamespace !== "peri-agent.task-terminal"
        || event.eventKind !== "taskTerminal" || event.eventId !== deliveryId
        || boundary.invocationIds.includes(event.causationId)) continue;
      const payload = JSON.parse(event.content.serialized) as {
        type: string;
        reminder: {
          source: string; kind: string; body: string;
          metadata: { task_id: string; child_thread_id: string | null };
        };
      };
      const binding = root.state.taskBindings[event.causationId];
      let outputMatches = payload.reminder.body.includes(marker);
      if (source === "shell") {
        const stdout = /^stdout 输出文件：(.+)$/m.exec(payload.reminder.body)?.[1];
        if (stdout) {
          outputMatches = path.isAbsolute(stdout) && existsSync(stdout)
            && payload.reminder.body.includes("退出码 0")
            && payload.reminder.body.includes("完整输出已保存到文件系统")
            && readFileSync(stdout, "utf8").includes(marker);
        }
      }
      if (payload.type !== "system_reminder" || payload.reminder.source !== source
        || payload.reminder.kind !== "completed" || !outputMatches
        || !binding || !Object.hasOwn(root.state.invocations, binding.invocationId)
        || binding.initiatorSessionId !== root.id
        || binding.recipientLifecycle !== delivery.recipientLifecycle
        || binding.ownerTaskId !== payload.reminder.metadata.task_id) continue;
      const obligation = root.state.obligations[deliveryId];
      const batch = delivery.batchId ? root.state.batches[delivery.batchId] : undefined;
      if (obligation?.status !== "satisfied" || !obligation.workId || !batch
        || root.state.works[obligation.workId]?.batchId !== delivery.batchId
        || batch.recipientLifecycle !== delivery.recipientLifecycle
        || !batch.processingDeliveryIds.includes(deliveryId)) continue;
      const owned = database.prepare(
        "SELECT c.command_json, r.resolution_json FROM session_work_commands c JOIN session_work_receipts r ON r.mutation_id=c.mutation_id AND r.session_id=c.session_id AND r.digest=c.digest WHERE c.session_id=? AND c.mutation_id=? AND c.reconciled=1",
      ).get(root.id, `task-terminal:${deliveryId}`) as { command_json: string; resolution_json: string } | undefined;
      if (!owned) continue;
      const command = JSON.parse(owned.command_json) as State["terminalObligations"][string];
      const resolution = JSON.parse(owned.resolution_json) as { status: string; receipt: Receipt };
      if (command.sessionId !== root.id || command.recipientLifecycle !== delivery.recipientLifecycle
        || JSON.stringify(command.action.binding) !== JSON.stringify(binding)
        || command.action.kind !== "publishTaskSettlement" || command.action.delivery.deliveryId !== deliveryId
        || resolution.status !== "applied" || resolution.receipt.decision.kind !== "accepted"
        || resolution.receipt.sessionId !== root.id || resolution.receipt.deliveryId !== deliveryId) continue;
      const storedEvent = database.prepare("SELECT event_json FROM session_work_events WHERE event_key=?")
        .get(JSON.stringify([event.producerNamespace, event.eventId])) as { event_json: string } | undefined;
      if (!storedEvent || JSON.stringify(JSON.parse(storedEvent.event_json)) !== JSON.stringify(event)) continue;
      const evidence: BgCompletionEvidence = {
        parentSessionId: root.id, invocationId: binding.invocationId, deliveryId, batchId: delivery.batchId!,
      };
      if (source === "shell") return evidence;
      if (!payload.reminder.metadata.child_thread_id) continue;
      const children = database.prepare(
        "SELECT t.id, s.state_json, c.state_json AS control_json FROM threads t JOIN session_work_state s ON s.session_id=t.id JOIN session_control_state c ON c.session_id=t.id WHERE t.id=? AND t.parent_thread_id=?",
      ).all(payload.reminder.metadata.child_thread_id, root.id) as {
        id: string; state_json: string; control_json: string;
      }[];
      for (const childRow of children) {
        const control = JSON.parse(childRow.control_json) as { attempt: unknown };
        if (control.attempt !== null) continue;
        const child = JSON.parse(childRow.state_json) as State;
        for (const [admissionId, terminal] of Object.entries(child.terminalObligations)) {
          const admission = child.admissions[admissionId];
          if (JSON.stringify(terminal) !== JSON.stringify(command)
            || JSON.stringify(child.terminalAcknowledgements[admissionId]) !== JSON.stringify(resolution.receipt)
            || admission?.settledReceipt?.decision.kind !== "accepted"
            || admission.admission.sessionId !== childRow.id
            || JSON.stringify(child.workDelegations[admission.admission.workId]) !== JSON.stringify(binding)) continue;
          return { ...evidence, childSessionId: childRow.id, childAdmissionId: admissionId };
        }
      }
    }
    return undefined;
  });
}
