import { millisToUnixSeconds, parseJsonColumn } from '../json';
import type { PlatformTraceRow } from '../repositories/traces';

/** Wire shape for `/v1/agents/sessions/{session_id}/traces`. */

export interface TraceResourceWire {
  readonly object: 'agent.session.trace';
  /** The root turn ID; rows without a turn keep their own platform ID as the anchor. */
  readonly id: string;
  readonly session_id: string;
  readonly created_at: number;
  readonly otlp: Readonly<Record<string, unknown>>;
}

function otlpDocument(raw: string): Record<string, unknown> | null {
  const parsed = parseJsonColumn<unknown>(raw, null);
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return null;
  return parsed as Record<string, unknown>;
}

/**
 * Maps stored trace rows to published resources. Rows whose payload is not an OTLP JSON document
 * are skipped rather than answered with a synthesized span, and a turn is published once.
 */
export function traceResources(rows: readonly PlatformTraceRow[]): TraceResourceWire[] {
  const published: TraceResourceWire[] = [];
  const seen = new Set<string>();
  for (const row of rows) {
    const otlp = otlpDocument(row.data);
    if (otlp === null) continue;
    const id = row.turn_id === null || row.turn_id.length === 0 ? row.id : row.turn_id;
    if (seen.has(id)) continue;
    seen.add(id);
    published.push({
      object: 'agent.session.trace',
      id,
      session_id: row.session_id,
      created_at: millisToUnixSeconds(row.created_at),
      otlp,
    });
  }
  return published;
}
