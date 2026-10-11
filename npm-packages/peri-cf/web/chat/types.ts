export type {
  Chat, EntryDTO as Entry, EntryBlockDTO as EntryBlock, ToolCallDTO as ToolCall, TurnDTO as Turn,
  InteractionDTO as Interaction, TaskDTO as Task, PlanEntryDTO as PlanEntry, InputQueueDTO as InputQueue,
  SessionInfoDTO as SessionInfo,
} from '../../shared/chat';
export type { SyncState, SyncReadiness } from '../../shared/sync';
export type { PayloadReadError, ToolPayloadReader, ToolPayloadResponse, ToolPayloadReadRef, InteractionFieldView, InteractionAnswer } from '@peri-code/sdk/view';

export type Phase = 'idle' | 'loading' | 'streaming' | 'cancelling' | 'syncing' | 'blocked';
