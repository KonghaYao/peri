export { messageStatuses } from '../../shared/chat';
export type { Chat, Message, ChatDetail } from '../../shared/chat';

export type Phase = 'idle' | 'loading' | 'streaming' | 'cancelling' | 'syncing' | 'blocked';
