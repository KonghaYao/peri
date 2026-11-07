import type { MessageRow } from '../storage/rows';

export interface MessagePreviewView {
  message_id: string;
  sender_id: string;
  created_seq: string;
  preview: string;
  body_bytes: number;
  created_at: string;
}

/** 历史、inbox 与资源共用的消息摘要投影；不包含正文。 */
export function messagePreviewOf(row: MessageRow | null): MessagePreviewView | null {
  if (!row) return null;
  return {
    message_id: row.message_id,
    sender_id: row.sender_id,
    created_seq: String(row.created_seq),
    preview: row.preview,
    body_bytes: row.body_bytes,
    created_at: String(row.created_at),
  };
}
