import { notificationBytes } from "../transport/notification-budget";
import { ChatViewReplayOverflowError } from "./errors";
import type { ChatBindingTarget, ChatFact, ChatFactEnvelope } from "../contracts/chat";

const MAX_FACT_COUNT = 4096;
const MAX_FACT_BYTES = 4 * 1024 * 1024;

/** 单个绑定代次的有界事实日志；溢出后只保留 live fan-out。 */
export class FactLog {
    private readonly envelopes: ChatFactEnvelope[] = [];
    private bytes = 0;
    private nextSequence = 1;
    overflowed = false;

    constructor(
        private readonly chatId: string,
        private readonly target: ChatBindingTarget,
    ) {}

    append(fact: ChatFact): ChatFactEnvelope {
        const envelope: ChatFactEnvelope = Object.freeze({
            chatId: this.chatId,
            sessionId: this.target.sessionId,
            bindingRevision: this.target.bindingRevision,
            sequence: this.nextSequence++,
            fact,
        });
        if (!this.overflowed) {
            const size = notificationBytes(fact, MAX_FACT_BYTES);
            if (
                this.envelopes.length >= MAX_FACT_COUNT ||
                this.bytes + size > MAX_FACT_BYTES
            ) {
                this.overflowed = true;
            } else {
                this.envelopes.push(envelope);
                this.bytes += size;
            }
        }
        return envelope;
    }

    replay(): readonly ChatFactEnvelope[] {
        if (this.overflowed) {
            throw new ChatViewReplayOverflowError(
                "Chat fact log overflowed; a new view must wait for a new binding",
            );
        }
        return [...this.envelopes];
    }
}
