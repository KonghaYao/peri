/** Conservative retained-size estimate without serializing a second copy of raw ACP data. */
export function notificationBytes(value: unknown, limit: number): number {
    const pending = [value];
    const seen = new Set<object>();
    let bytes = 0;
    while (pending.length && bytes <= limit) {
        const part = pending.pop();
        if (typeof part === "string") bytes += part.length * 2;
        else if (part && typeof part === "object" && !seen.has(part)) {
            seen.add(part);
            bytes += 32;
            if (Array.isArray(part)) {
                bytes += part.length * 8;
                if (bytes <= limit) for (const child of part) pending.push(child);
            } else {
                for (const [key, child] of Object.entries(part)) {
                    bytes += key.length * 2 + 16;
                    if (bytes > limit) break;
                    pending.push(child);
                }
            }
        } else bytes += 8;
    }
    return bytes;
}

export class EventStreamOverflowError extends Error {
    constructor() {
        super("Raw ACP event stream exceeded its buffer budget; read SessionDocs for current state or open a new live stream");
        this.name = "EventStreamOverflowError";
    }
}
