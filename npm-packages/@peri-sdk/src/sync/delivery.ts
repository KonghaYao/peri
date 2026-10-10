import { encodeSyncFrame, type SyncFramePayload } from "./wire";

/**
 * Delivery credit for one ordered read-only replication transport. The producer may not run ahead of
 * the peer's acknowledgements: unacknowledged frames are bounded by count and bytes, and each
 * reservation carries the deadline after which the peer is treated as a slow consumer.
 */

export type DeliveryReservation = { delivery: number; bytes: number; deadline: number };

export interface DeliveryLedger {
    nextDelivery: number;
    acknowledged: number;
    pending: DeliveryReservation[];
}

export interface DeliveryCreditPolicy {
    maxFrames: number;
    maxBytes: number;
    timeoutMs: number;
    now?: () => number;
}

export function reserveDeliveryFrame(ledger: DeliveryLedger, frame: SyncFramePayload,
    policy: DeliveryCreditPolicy): Uint8Array<ArrayBuffer> {
    const delivery = ledger.nextDelivery;
    if (!Number.isSafeInteger(delivery + 1)) throw new Error("Delivery sequence exhausted");
    const encoded = encodeSyncFrame({ ...frame, delivery });
    const bytes = encoded.byteLength;
    const outstanding = ledger.pending.reduce((total, pending) => total + pending.bytes, 0);
    if (ledger.pending.length >= policy.maxFrames || outstanding + bytes > policy.maxBytes)
        throw new Error("Sync peer exceeded outstanding delivery credit");
    ledger.pending.push({ delivery, bytes, deadline: (policy.now?.() ?? Date.now()) + policy.timeoutMs });
    ledger.nextDelivery++;
    return encoded;
}

export function acknowledgeDelivery(ledger: DeliveryLedger, delivery: number): void {
    if (ledger.pending[0]?.delivery !== delivery) throw new Error("Unexpected delivery acknowledgement");
    ledger.pending.shift();
    ledger.acknowledged = delivery;
}

/** Stable fingerprint that binds a persisted connection attachment to a credential without storing it. */
export async function credentialFingerprint(token: string): Promise<string> {
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token));
    return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}
