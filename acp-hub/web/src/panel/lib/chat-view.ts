import * as Y from 'yjs';
import { asArray, asMap, getNum, getStr, safeTime, yText, yValue } from './yjs-values';

export interface ReasoningBlock { text: string; visibility: string | null }
export interface ToolCallInfo {
  toolCallId: string | null;
  name: string | null;
  status: string | null;
  arguments: unknown;
  result: unknown;
  resultOmitted: boolean | null;
  resultBytes: number | null;
  publicError: { code: string | null; message: string | null } | null;
  startedAt: string | null;
  completedAt: string | null;
}
export interface ResourceInfo { resourceId: string | null; mediaType: string | null; name: string | null }
export interface ChatEntry {
  id: string;
  turnId: string | null;
  kind: string | null;
  role: string | null;
  status: string | null;
  authorUserId: string | null;
  createdAt: string;
  completedAt: string | null;
  text: string;
  reasoning: ReasoningBlock[];
  toolCalls: ToolCallInfo[];
  resources: ResourceInfo[];
  error: { code: string | null; message: string | null } | null;
}
export interface ChatView { schemaVersion: unknown; projectionVersion: unknown; entries: ChatEntry[] }

/** Read-only browser projection of one chat:{id} document. */
export function renderChat(doc: Y.Doc): ChatView {
  const root = doc.getMap<unknown>('root');
  const order = asArray(root.get('entry_order'));
  const entriesMap = asMap(root.get('entries'));
  const toolCalls = asMap(root.get('tool_calls'));
  const entries: ChatEntry[] = [];
  const referencedToolIds = new Set<string>();

  const readToolCall = (id: string, map: Y.Map<unknown> | null): ToolCallInfo => ({
    toolCallId: id,
    name: getStr(map, 'name'),
    status: getStr(map, 'status'),
    arguments: yValue(map?.get('arguments')),
    result: yValue(map?.get('result')),
    resultOmitted: map?.has('result_omitted') ? map.get('result_omitted') === true : null,
    resultBytes: getNum(map, 'result_bytes'),
    publicError: (() => {
      const error = asMap(map?.get('public_error'));
      return error ? { code: getStr(error, 'code'), message: getStr(error, 'message') } : null;
    })(),
    startedAt: getStr(map, 'started_at'),
    completedAt: getStr(map, 'completed_at'),
  });

  order?.toArray().forEach((entryIdValue) => {
    if (typeof entryIdValue !== 'string') return;
    const map = asMap(entriesMap?.get(entryIdValue));
    if (!map) return;
    const entry: ChatEntry = {
      id: entryIdValue,
      turnId: getStr(map, 'turn_id'),
      kind: getStr(map, 'kind'),
      role: getStr(map, 'role'),
      status: getStr(map, 'status'),
      authorUserId: getStr(map, 'author_user_id'),
      createdAt: safeTime(map.get('created_at')),
      completedAt: getStr(map, 'completed_at'),
      text: '',
      reasoning: [],
      toolCalls: [],
      resources: [],
      error: null,
    };
    const error = asMap(map.get('error'));
    if (error) entry.error = { code: getStr(error, 'code'), message: getStr(error, 'message') };

    const blocks = asMap(map.get('blocks'));
    asArray(map.get('block_order'))?.toArray().forEach((blockIdValue) => {
      if (typeof blockIdValue !== 'string') return;
      const block = asMap(blocks?.get(blockIdValue));
      if (!block) return;
      switch (block.get('kind')) {
        case 'text': {
          const text = yText(block.get('text'));
          if (text !== null) entry.text += text;
          break;
        }
        case 'reasoning':
          entry.reasoning.push({ text: yText(block.get('text')) || '', visibility: getStr(block, 'visibility') });
          break;
        case 'tool_call': {
          const id = getStr(block, 'tool_call_id');
          if (id) referencedToolIds.add(id);
          entry.toolCalls.push(readToolCall(id || '', asMap(id ? toolCalls?.get(id) : null)));
          break;
        }
        case 'resource':
          entry.resources.push({
            resourceId: getStr(block, 'resource_id'),
            mediaType: getStr(block, 'media_type'),
            name: getStr(block, 'name'),
          });
          break;
        default:
          break;
      }
    });
    entries.push(entry);
  });

  const assistantByTurn = new Map(entries
    .filter((entry) => entry.role === 'assistant' && entry.turnId)
    .map((entry) => [entry.turnId as string, entry]));
  const legacyOrphans: Array<{ id: string; turnId: string; startedAt: string; map: Y.Map<unknown> }> = [];
  toolCalls?.forEach((value, id) => {
    if (referencedToolIds.has(id)) return;
    const map = asMap(value);
    const turnId = getStr(map, 'turn_id');
    if (!map || !turnId || !assistantByTurn.has(turnId)) return;
    legacyOrphans.push({ id, turnId, startedAt: getStr(map, 'started_at') || '', map });
  });
  legacyOrphans
    .sort((left, right) => left.startedAt.localeCompare(right.startedAt) || left.id.localeCompare(right.id))
    .forEach((orphan) => assistantByTurn.get(orphan.turnId)?.toolCalls.push(readToolCall(orphan.id, orphan.map)));

  return {
    schemaVersion: root.get('schema_version'),
    projectionVersion: root.get('projection_version'),
    entries,
  };
}
