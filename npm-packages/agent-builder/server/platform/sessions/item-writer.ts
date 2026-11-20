import { newPlatformId } from '../ids';
import { parseJsonColumn } from '../json';
import type { PlatformRepositories } from '../repositories';
import {
  accumulateUsage,
  asRecord,
  type Block,
  commandOutputOf,
  type Entry,
  entryStatus,
  type ItemState,
  type ItemStatus,
  isTerminalEntry,
  isTerminalTool,
  itemTypeForTool,
  type PartState,
  sourceKeyOf,
  stripInternal,
  type Tool,
  type TurnState,
  textOf,
  toolPayload,
  toolStatus,
} from './diff';
import {
  commandOutputDeltaEvent,
  contentPartEvent,
  itemEvent,
  newEventId,
  outputTextDeltaEvent,
  outputTextDoneEvent,
  reasoningPartEvent,
  reasoningTextDeltaEvent,
  reasoningTextDoneEvent,
  type SessionEventData,
} from './events';
import type { SessionItemWire } from './serialize';

export interface ItemRefs {
  readonly turnId?: string | null;
  readonly itemId?: string | null;
}

export interface ItemWriterOptions {
  readonly repos: PlatformRepositories;
  readonly now: () => number;
  readonly emit: (sessionId: string, data: SessionEventData, refs: ItemRefs) => void;
}

/**
 * Writes the per-turn item timeline: one item per message entry, reasoning block and tool call.
 *
 * Item identity is stable across snapshots (`entry:<id>`, `reason:<entry>:<block>`, `tool:<id>`),
 * text is appended by prefix diff, and a shorter text closes the current part before a new one
 * opens. Silent items are the terminal history observed at attach time: rows are written, no events
 * are replayed.
 */
export class SessionItemWriter {
  readonly #repos: PlatformRepositories;
  readonly #now: () => number;
  readonly #emit: ItemWriterOptions['emit'];

  constructor(options: ItemWriterOptions) {
    this.#repos = options.repos;
    this.#now = options.now;
    this.#emit = options.emit;
  }

  /** Rebuilds the in-memory item map of a turn that already has rows. */
  hydrate(turn: TurnState): void {
    let maxIndex = -1;
    for (const stored of this.#repos.items.listByTurn(turn.id)) {
      const payload = parseJsonColumn<Record<string, unknown>>(stored.payload, {});
      const sourceKey = sourceKeyOf(payload, stored.call_id);
      if (sourceKey === null) continue;
      const parts = new Map<string, PartState>();
      for (const [key, index] of Object.entries(asRecord(payload._parts))) {
        if (typeof index !== 'number') continue;
        parts.set(key, {
          key,
          index,
          text: partText(payload, stored.type, index),
          open: false,
        });
      }
      turn.items.set(sourceKey, {
        id: stored.id,
        type: stored.type,
        sourceKey,
        outputIndex: stored.output_index,
        silent: true,
        status:
          stored.status === 'completed'
            ? 'completed'
            : stored.status === 'incomplete'
              ? 'incomplete'
              : 'in_progress',
        payload,
        parts,
        done: stored.status !== 'in_progress',
      });
      maxIndex = Math.max(maxIndex, stored.output_index);
    }
    turn.nextOutputIndex = maxIndex + 1;
  }

  syncEntry(sessionId: string, turn: TurnState, entry: Entry, seed: boolean): void {
    if (entry.kind !== 'message') return;
    accumulateUsage(turn, entry);
    this.#syncMessageItem(sessionId, turn, entry, seed);
    if (entry.role !== 'assistant') return;
    for (const block of entry.blocks) {
      if (block.type !== 'reasoning') continue;
      this.#syncReasoningItem(sessionId, turn, entry, block, seed);
    }
  }

  syncTool(sessionId: string, turn: TurnState, tool: Tool, seed: boolean): void {
    const key = `tool:${tool.toolCallId}`;
    const type = itemTypeForTool(tool);
    const status = toolStatus(tool);
    const silent = seed && isTerminalTool(tool);
    let item = turn.items.get(key);
    if (item === undefined) {
      item = this.#createItem(
        sessionId,
        turn,
        key,
        { ...toolPayload(tool, type), _call_id: tool.toolCallId },
        status,
        silent,
      );
    }
    if (item.done) return;
    if (item.silent) {
      this.#patchItem(item, toolPayload(tool, type));
      this.finish(sessionId, turn, item, status);
      return;
    }
    if (type === 'command_execution') this.#streamCommandOutput(sessionId, turn, item, tool);
    this.#patchItem(item, toolPayload(tool, type));
    if (isTerminalTool(tool)) this.finish(sessionId, turn, item, status);
  }

  /** Closes every open part, records the terminal status and emits `item.done` once. */
  finish(sessionId: string, turn: TurnState, item: ItemState, status: ItemStatus): void {
    if (item.done) return;
    if (item.type === 'message' || item.type === 'reasoning') {
      for (const part of [...item.parts.values()]) this.#closePart(sessionId, turn, item, part);
      this.#writeParts(item);
    }
    item.status = status;
    item.done = true;
    item.payload.status = status;
    this.#repos.items.update(item.id, {
      status,
      payload: JSON.stringify(item.payload),
      completedAt: this.#now(),
    });
    if (item.silent) return;
    this.#emit(
      sessionId,
      itemEvent(
        'agent.session.turn.item.done',
        newEventId(),
        sessionId,
        turn.id,
        item.outputIndex,
        wireItem(item),
      ),
      { turnId: turn.id, itemId: item.id },
    );
  }

  #syncMessageItem(sessionId: string, turn: TurnState, entry: Entry, seed: boolean): void {
    const key = `entry:${entry.entryId}`;
    const user = entry.role === 'user';
    const status = entryStatus(entry);
    const silent = seed && isTerminalEntry(entry);
    let item = turn.items.get(key);
    if (item === undefined) {
      const text = textOf(entry);
      item = this.#createItem(
        sessionId,
        turn,
        key,
        {
          type: 'message',
          role: entry.role,
          content: user && text.length > 0 ? [{ type: 'input_text', text }] : [],
          status,
          phase: null,
          _entry_id: entry.entryId,
        },
        status,
        silent,
      );
    }
    if (item.done) return;

    if (item.silent) {
      this.#absorbMessage(item, entry);
      this.finish(sessionId, turn, item, status);
      return;
    }
    if (user) {
      const text = textOf(entry);
      this.#patchItem(item, {
        content: text.length === 0 ? [] : [{ type: 'input_text', text }],
        status,
      });
    } else {
      for (const block of entry.blocks) {
        if (block.type === 'text') this.#streamTextPart(sessionId, turn, item, block.blockId, block.text);
      }
      this.#patchItem(item, { status });
    }
    if (isTerminalEntry(entry)) this.finish(sessionId, turn, item, status);
  }

  #syncReasoningItem(sessionId: string, turn: TurnState, entry: Entry, block: Block, seed: boolean): void {
    if (block.type !== 'reasoning') return;
    const key = `reason:${entry.entryId}:${block.blockId}`;
    const status = entryStatus(entry);
    const silent = seed && isTerminalEntry(entry);
    let item = turn.items.get(key);
    if (item === undefined) {
      item = this.#createItem(
        sessionId,
        turn,
        key,
        {
          type: 'reasoning',
          summary: [],
          status,
          _entry_id: entry.entryId,
          _block_id: block.blockId,
        },
        status,
        silent,
      );
    }
    if (item.done) return;
    if (item.silent) {
      item.payload.summary = [{ type: 'summary_text', text: block.text }];
      this.finish(sessionId, turn, item, status);
      return;
    }
    this.#streamReasoningPart(sessionId, turn, item, block.text);
    this.#patchItem(item, { status });
    if (isTerminalEntry(entry)) this.finish(sessionId, turn, item, status);
  }

  #streamTextPart(sessionId: string, turn: TurnState, item: ItemState, blockId: string, next: string): void {
    const key = `text:${blockId}`;
    let part = item.parts.get(key);
    if (part === undefined || !next.startsWith(part.text)) {
      if (part !== undefined) this.#closePart(sessionId, turn, item, part);
      part = this.#openPart(sessionId, turn, item, key, nextPartIndex(item));
    }
    const delta = next.slice(part.text.length);
    if (delta.length > 0) this.#appendTextDelta(sessionId, turn, item, part, delta);
  }

  #openPart(sessionId: string, turn: TurnState, item: ItemState, key: string, index: number): PartState {
    const part: PartState = { key, index, text: '', open: true };
    item.parts.set(key, part);
    item.payload._parts = { ...asRecord(item.payload._parts), [key]: index };
    if (item.type === 'reasoning') {
      item.payload.summary = [...summaryArray(item), { type: 'summary_text', text: '' }];
      this.#emitPart(sessionId, 'reasoning.added', turn, item, index);
      return part;
    }
    item.payload.content = contentArray(item);
    this.#emitPart(sessionId, 'text.added', turn, item, index);
    return part;
  }

  #appendTextDelta(
    sessionId: string,
    turn: TurnState,
    item: ItemState,
    part: PartState,
    delta: string,
  ): void {
    part.text += delta;
    this.#writeParts(item);
    this.#persist(item);
    if (item.silent) return;
    this.#emit(
      sessionId,
      outputTextDeltaEvent(newEventId(), sessionId, turn.id, item.id, item.outputIndex, part.index, delta),
      { turnId: turn.id, itemId: item.id },
    );
  }

  #streamReasoningPart(sessionId: string, turn: TurnState, item: ItemState, next: string): void {
    let part = item.parts.get('summary:0');
    if (part === undefined || !next.startsWith(part.text)) {
      const index = part === undefined ? 0 : part.index + 1;
      if (part !== undefined) this.#closePart(sessionId, turn, item, part);
      part = this.#openPart(sessionId, turn, item, 'summary:0', index);
    }
    const delta = next.slice(part.text.length);
    if (delta.length === 0) return;
    part.text += delta;
    this.#writeParts(item);
    this.#persist(item);
    if (item.silent) return;
    this.#emit(
      sessionId,
      reasoningTextDeltaEvent(newEventId(), sessionId, turn.id, item.id, item.outputIndex, part.index, delta),
      { turnId: turn.id, itemId: item.id },
    );
  }

  #closePart(sessionId: string, turn: TurnState, item: ItemState, part: PartState): void {
    if (!part.open) return;
    part.open = false;
    if (item.silent) return;
    if (item.type === 'reasoning') {
      this.#emit(
        sessionId,
        reasoningTextDoneEvent(
          newEventId(),
          sessionId,
          turn.id,
          item.id,
          item.outputIndex,
          part.index,
          part.text,
        ),
        { turnId: turn.id, itemId: item.id },
      );
      this.#emit(
        sessionId,
        reasoningPartEvent(
          'agent.session.turn.reasoning_summary_part.done',
          newEventId(),
          sessionId,
          turn.id,
          item.id,
          item.outputIndex,
          part.index,
          part.text,
        ),
        { turnId: turn.id, itemId: item.id },
      );
      return;
    }
    this.#emit(
      sessionId,
      outputTextDoneEvent(newEventId(), sessionId, turn.id, item.id, item.outputIndex, part.index, part.text),
      { turnId: turn.id, itemId: item.id },
    );
    this.#emit(
      sessionId,
      contentPartEvent(
        'agent.session.turn.content_part.done',
        newEventId(),
        sessionId,
        turn.id,
        item.id,
        item.outputIndex,
        part.index,
        part.text,
      ),
      { turnId: turn.id, itemId: item.id },
    );
  }

  #emitPart(
    sessionId: string,
    kind: 'text.added' | 'reasoning.added',
    turn: TurnState,
    item: ItemState,
    index: number,
  ): void {
    if (item.silent) return;
    const data =
      kind === 'text.added'
        ? contentPartEvent(
            'agent.session.turn.content_part.added',
            newEventId(),
            sessionId,
            turn.id,
            item.id,
            item.outputIndex,
            index,
            '',
          )
        : reasoningPartEvent(
            'agent.session.turn.reasoning_summary_part.added',
            newEventId(),
            sessionId,
            turn.id,
            item.id,
            item.outputIndex,
            index,
            '',
          );
    this.#emit(sessionId, data, { turnId: turn.id, itemId: item.id });
  }

  #streamCommandOutput(sessionId: string, turn: TurnState, item: ItemState, tool: Tool): void {
    const previous = typeof item.payload.output === 'string' ? item.payload.output : '';
    const next = commandOutputOf(tool);
    if (next === previous) return;
    const delta = next.startsWith(previous) ? next.slice(previous.length) : next;
    if (delta.length === 0) return;
    this.#emit(
      sessionId,
      commandOutputDeltaEvent(newEventId(), sessionId, turn.id, item.id, item.outputIndex, delta),
      { turnId: turn.id, itemId: item.id },
    );
  }

  #createItem(
    sessionId: string,
    turn: TurnState,
    sourceKey: string,
    payload: Record<string, unknown>,
    status: ItemStatus,
    silent: boolean,
  ): ItemState {
    const id = newPlatformId();
    const outputIndex = turn.nextOutputIndex;
    turn.nextOutputIndex += 1;
    const stored = { ...payload, status };
    const row = this.#repos.items.create({
      id,
      sessionId,
      turnId: turn.id,
      outputIndex,
      type: String(payload.type),
      status,
      callId: typeof payload.call_id === 'string' ? payload.call_id : null,
      payload: JSON.stringify(stored),
    });
    const item: ItemState = {
      id,
      type: row.type,
      sourceKey,
      outputIndex,
      status,
      payload: stored,
      parts: new Map(),
      done: false,
      silent,
    };
    turn.items.set(sourceKey, item);
    if (!silent) {
      this.#emit(
        sessionId,
        itemEvent(
          'agent.session.turn.item.added',
          newEventId(),
          sessionId,
          turn.id,
          outputIndex,
          wireItem(item),
        ),
        { turnId: turn.id, itemId: item.id },
      );
    }
    return item;
  }

  #patchItem(item: ItemState, patch: Record<string, unknown>): void {
    let changed = false;
    for (const [key, value] of Object.entries(patch)) {
      if (JSON.stringify(item.payload[key]) === JSON.stringify(value)) continue;
      item.payload[key] = value;
      changed = true;
    }
    if (changed) this.#persist(item);
  }

  #absorbMessage(item: ItemState, entry: Entry): void {
    const text = textOf(entry);
    if (item.payload.role === 'user') {
      item.payload.content = text.length === 0 ? [] : [{ type: 'input_text', text }];
      return;
    }
    const content: { type: 'output_text'; text: string }[] = [];
    const parts: Record<string, number> = {};
    for (const block of entry.blocks) {
      if (block.type !== 'text') continue;
      parts[`text:${block.blockId}`] = content.length;
      content.push({ type: 'output_text', text: block.text });
    }
    item.payload.content = content;
    item.payload._parts = { ...asRecord(item.payload._parts), ...parts };
  }

  #writeParts(item: ItemState): void {
    const ordered = [...item.parts.values()].sort((left, right) => left.index - right.index);
    if (item.type === 'reasoning') {
      item.payload.summary = ordered.map((part) => ({ type: 'summary_text', text: part.text }));
      return;
    }
    item.payload.content = ordered.map((part) => ({ type: 'output_text', text: part.text }));
  }

  #persist(item: ItemState): void {
    this.#repos.items.update(item.id, { status: item.status, payload: JSON.stringify(item.payload) });
  }
}

/** Strips recorder-internal keys before an item crosses the wire. */
export function wireItem(item: ItemState): SessionItemWire {
  return stripInternal({ ...item.payload, id: item.id, type: item.type }) as SessionItemWire;
}

function nextPartIndex(item: ItemState): number {
  let max = -1;
  for (const part of item.parts.values()) max = Math.max(max, part.index);
  return max + 1;
}

function contentArray(item: ItemState): unknown[] {
  const content = item.payload.content;
  return Array.isArray(content) ? [...content] : [];
}

function summaryArray(item: ItemState): unknown[] {
  const summary = item.payload.summary;
  return Array.isArray(summary) ? [...summary] : [];
}

function partText(payload: Record<string, unknown>, type: string, index: number): string {
  const source = type === 'reasoning' ? payload.summary : payload.content;
  if (!Array.isArray(source) || index >= source.length) return '';
  const part = source[index];
  return typeof part === 'object' && part !== null && typeof (part as { text?: unknown }).text === 'string'
    ? (part as { text: string }).text
    : '';
}
