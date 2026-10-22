import type { Entry, ToolCall } from '../runtime';

/** 工作项：reasoning 与 tool 在时间线上合并为连续组。 */
export interface TimelineWorkItem {
  kind: 'reasoning' | 'tool';
  /** 稳定身份：reasoning 用 `entryId:blockId`，tool 优先用 toolCallId。 */
  id: string;
  entryId: string;
  blockId: string;
  toolCallId?: string;
  status: string;
  /** 展示标题，例如「运行命令」「思考」。 */
  label: string;
  /** 展示目标，例如命令、路径或搜索词。 */
  target?: string;
  /** reasoning 正文；工具项不使用。 */
  text?: string;
  createdAt: string | null;
  completedAt: string | null;
  failed: boolean;
  /** 工具记录缺失时的原因；不静默丢弃调用身份。 */
  unavailable?: string;
  tool?: ToolCall;
}

export type TimelineRow =
  | { kind: 'user'; id: string; entry: Entry }
  | {
    kind: 'assistant'; id: string; entryId: string; text: string;
    streaming: boolean; status: string; createdAt: string | null;
  }
  | {
    kind: 'work'; id: string; turnId: string | null; items: TimelineWorkItem[]; active: boolean;
    /** 组内存在失败工具：摘要与展开都保留失败信号。 */
    failed: boolean;
    /** 首帧是否展开：运行中、组内失败或失败/取消 turn 的 work 默认可见。 */
    defaultExpanded: boolean;
    /** 状态转为失败终态时即使曾手动折叠也展开一次。 */
    forceExpanded: boolean;
  }
  | {
    kind: 'turn-fold'; id: string; turnId: string; status: string; label: string;
    failed: boolean;
    startedAt: string | null; completedAt: string | null; expanded: boolean;
  }
  | { kind: 'working'; id: string; turnId: string | null; startedAt: string | null }
  | { kind: 'error'; id: string; entryId: string; message: string }
  | { kind: 'unknown'; id: string; entryId: string; blockId: string; reason: string };

export type TimelineRowKind = TimelineRow['kind'];
