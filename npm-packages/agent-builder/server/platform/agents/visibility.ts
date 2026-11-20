import { SESSION_INLINE_AGENT_ID } from '../../db/migrate';

/**
 * Reserved agent rows that back internal machinery rather than user-created agents.
 * Migration v3 seeds one for inline sessions; it shares the `agents` table but is not a
 * resource the platform API exposes.
 */
const HIDDEN_AGENT_IDS: ReadonlySet<string> = new Set([SESSION_INLINE_AGENT_ID]);

export function isHiddenAgentId(id: string): boolean {
  return HIDDEN_AGENT_IDS.has(id);
}
