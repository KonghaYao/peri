import { createDecoder, hasContent, readUint8, readUint8Array, readVarUint, type Decoder } from "lib0/decoding";
import { createEncoder, toUint8Array, writeUint8, writeVarUint, writeVarUint8Array, type Encoder } from "lib0/encoding";
import type { DocSnapshot, DocStateVector, DocUpdate } from "./session-doc-sync";

/**
 * Binary wire for read-only document replication: one WebSocket message carries exactly one frame,
 * `tag:u8, wireVersion:u8, body`. Document bytes and state vectors stay `Uint8Array`; delivery
 * numbering is transport identity and is never confused with the document generation/sequence.
 */

export const SYNC_WIRE_VERSION = 1;
export const FRAME_AUTH = 0x01;
export const FRAME_SNAPSHOT = 0x02;
export const FRAME_UPDATE = 0x03;
export const FRAME_ACK = 0x04;
const SNAPSHOT_FULL = 0;
const SNAPSHOT_DELTA = 1;
const CHAT_BIT = 0b01;
const SESSION_BIT = 0b10;
const PROTOCOL_V2 = 2;

export const MAX_SYNC_FRAME_BYTES = 16 * 1024 * 1024;
export const MAX_AUTH_FRAME_BYTES = 192 * 1024;
export const MAX_ACK_FRAME_BYTES = 128;
const MAX_DOCUMENT_BYTES = 4 * 1024 * 1024;
const MAX_VECTOR_BYTES = 64 * 1024;
const MAX_TOKEN_CHARS = 4096;
const MAX_TOKEN_BYTES = MAX_TOKEN_CHARS * 4;
const MAX_GENERATION_CHARS = 128;
const MAX_GENERATION_BYTES = MAX_GENERATION_CHARS * 4;

const INVALID_SYNC = "Invalid document sync frame";
const INVALID_AUTH = "Invalid document sync authentication";
const INVALID_ACK = "Invalid document delivery acknowledgement";
const FRAME_BUDGET = "Document sync frame exceeds byte budget";

/** Snapshot/update before the transport assigns its delivery number. */
export type SyncFramePayload = { type: "snapshot"; snapshot: DocSnapshot } | { type: "update"; update: DocUpdate };
export type SyncFrame = SyncFramePayload & { delivery: number };
export type AuthFrame = { type: "auth"; token: string; resume?: DocStateVector };
export type AckFrame = { type: "ack"; delivery: number };

const utf8Encoder = new TextEncoder();
const utf8Decoder = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true });

function fail(message: string): never {
    throw new Error(message);
}

/** Normalize an inbound WebSocket message into frame bytes; text and other payloads are invalid. */
export function frameBytes(raw: unknown): Uint8Array | undefined {
    if (raw instanceof ArrayBuffer) return new Uint8Array(raw);
    if (ArrayBuffer.isView(raw)) return new Uint8Array(raw.buffer, raw.byteOffset, raw.byteLength);
    return undefined;
}

function requiredBytes(raw: unknown, maximum: number, message: string): Uint8Array {
    const bytes = frameBytes(raw);
    if (!bytes || bytes.byteLength > maximum) return fail(message);
    return bytes;
}

function remaining(decoder: Decoder): number {
    return decoder.arr.length - decoder.pos;
}

function readCount(decoder: Decoder, maximum: number, message: string): number {
    const value = readVarUint(decoder);
    if (!Number.isSafeInteger(value) || value > maximum) return fail(message);
    return value;
}

function readBlob(decoder: Decoder, maximum: number, message: string): Uint8Array {
    const length = readCount(decoder, maximum, message);
    if (length > remaining(decoder)) return fail(message);
    return readUint8Array(decoder, length);
}

function readText(decoder: Decoder, maximum: number, message: string): string {
    const bytes = readBlob(decoder, maximum, message);
    try { return utf8Decoder.decode(bytes); }
    catch { return fail(message); }
}

function writeText(encoder: Encoder, value: string): void {
    writeVarUint8Array(encoder, utf8Encoder.encode(value));
}

function decodeFrame<Value>(raw: unknown, maximum: number, message: string, read: (decoder: Decoder) => Value): Value {
    const decoder = createDecoder(requiredBytes(raw, maximum, message));
    try {
        const value = read(decoder);
        if (hasContent(decoder)) return fail(message);
        return value;
    } catch { return fail(message); }
}

function isSafeCount(value: unknown, minimum: number): value is number {
    return typeof value === "number" && Number.isSafeInteger(value) && value >= minimum;
}

function validGeneration(value: unknown): value is string {
    return typeof value === "string" && value.length >= 1 && value.length <= MAX_GENERATION_CHARS;
}

function validToken(value: unknown): value is string {
    return typeof value === "string" && value.length >= 1 && value.length <= MAX_TOKEN_CHARS;
}

function validDocument(value: unknown): value is Uint8Array {
    return value instanceof Uint8Array && value.byteLength <= MAX_DOCUMENT_BYTES;
}

function validVector(value: unknown): value is DocStateVector {
    if (typeof value !== "object" || value === null) return false;
    const vector = value as DocStateVector;
    return vector.protocol === PROTOCOL_V2 && validGeneration(vector.generation) &&
        vector.chat instanceof Uint8Array && vector.chat.byteLength <= MAX_VECTOR_BYTES &&
        vector.session instanceof Uint8Array && vector.session.byteLength <= MAX_VECTOR_BYTES;
}

export function decodeSyncFrame(raw: unknown): SyncFrame {
    return decodeFrame<SyncFrame>(raw, MAX_SYNC_FRAME_BYTES, INVALID_SYNC, (decoder): SyncFrame => {
        const tag = readUint8(decoder);
        if (readUint8(decoder) !== SYNC_WIRE_VERSION || (tag !== FRAME_SNAPSHOT && tag !== FRAME_UPDATE)) return fail(INVALID_SYNC);
        const delivery = readCount(decoder, Number.MAX_SAFE_INTEGER, INVALID_SYNC);
        if (delivery < 1 || readVarUint(decoder) !== PROTOCOL_V2) return fail(INVALID_SYNC);
        const generation = readText(decoder, MAX_GENERATION_BYTES, INVALID_SYNC);
        if (!validGeneration(generation)) return fail(INVALID_SYNC);
        const sequence = readCount(decoder, Number.MAX_SAFE_INTEGER, INVALID_SYNC);
        if (tag === FRAME_SNAPSHOT) {
            const mode = readUint8(decoder);
            if (mode !== SNAPSHOT_FULL && mode !== SNAPSHOT_DELTA) return fail(INVALID_SYNC);
            const chat = readBlob(decoder, MAX_DOCUMENT_BYTES, INVALID_SYNC);
            const session = readBlob(decoder, MAX_DOCUMENT_BYTES, INVALID_SYNC);
            return { type: "snapshot", delivery, snapshot: { protocol: PROTOCOL_V2, generation, sequence,
                mode: mode === SNAPSHOT_FULL ? "snapshot" : "delta", chat, session } };
        }
        const mask = readUint8(decoder);
        if (mask < 1 || (mask & ~(CHAT_BIT | SESSION_BIT)) !== 0) return fail(INVALID_SYNC);
        const chat = mask & CHAT_BIT ? readBlob(decoder, MAX_DOCUMENT_BYTES, INVALID_SYNC) : undefined;
        const session = mask & SESSION_BIT ? readBlob(decoder, MAX_DOCUMENT_BYTES, INVALID_SYNC) : undefined;
        return { type: "update", delivery, update: { protocol: PROTOCOL_V2, generation, sequence,
            ...(chat ? { chat } : {}), ...(session ? { session } : {}) } };
    });
}

export function encodeSyncFrame(frame: SyncFrame): Uint8Array<ArrayBuffer> {
    if (frame.type !== "snapshot" && frame.type !== "update") fail(INVALID_SYNC);
    const source = frame.type === "snapshot" ? frame.snapshot : frame.update;
    if (!isSafeCount(frame.delivery, 1) || source.protocol !== PROTOCOL_V2 ||
        !validGeneration(source.generation) || !isSafeCount(source.sequence, 0)) fail(INVALID_SYNC);
    const encoder = createEncoder();
    writeUint8(encoder, frame.type === "snapshot" ? FRAME_SNAPSHOT : FRAME_UPDATE);
    writeUint8(encoder, SYNC_WIRE_VERSION);
    writeVarUint(encoder, frame.delivery);
    writeVarUint(encoder, source.protocol);
    writeText(encoder, source.generation);
    writeVarUint(encoder, source.sequence);
    if (frame.type === "snapshot") {
        const mode = frame.snapshot.mode;
        if (mode !== "snapshot" && mode !== "delta") fail(INVALID_SYNC);
        if (!validDocument(frame.snapshot.chat) || !validDocument(frame.snapshot.session)) fail(INVALID_SYNC);
        writeUint8(encoder, mode === "snapshot" ? SNAPSHOT_FULL : SNAPSHOT_DELTA);
        writeVarUint8Array(encoder, frame.snapshot.chat);
        writeVarUint8Array(encoder, frame.snapshot.session);
    } else {
        const chat = frame.update.chat;
        const session = frame.update.session;
        if ((chat === undefined && session === undefined) || (chat !== undefined && !validDocument(chat)) ||
            (session !== undefined && !validDocument(session))) fail(INVALID_SYNC);
        writeUint8(encoder, (chat ? CHAT_BIT : 0) | (session ? SESSION_BIT : 0));
        if (chat) writeVarUint8Array(encoder, chat);
        if (session) writeVarUint8Array(encoder, session);
    }
    const bytes = toUint8Array(encoder);
    if (bytes.byteLength > MAX_SYNC_FRAME_BYTES) fail(FRAME_BUDGET);
    return bytes;
}

export function decodeAuthFrame(raw: unknown): AuthFrame {
    return decodeFrame<AuthFrame>(raw, MAX_AUTH_FRAME_BYTES, INVALID_AUTH, (decoder): AuthFrame => {
        if (readUint8(decoder) !== FRAME_AUTH || readUint8(decoder) !== SYNC_WIRE_VERSION) return fail(INVALID_AUTH);
        const token = readText(decoder, MAX_TOKEN_BYTES, INVALID_AUTH);
        if (!validToken(token)) return fail(INVALID_AUTH);
        const hasResume = readUint8(decoder);
        if (hasResume === 0) return { type: "auth", token };
        if (hasResume !== 1) return fail(INVALID_AUTH);
        const protocol = readVarUint(decoder);
        const generation = readText(decoder, MAX_GENERATION_BYTES, INVALID_AUTH);
        const chat = readBlob(decoder, MAX_VECTOR_BYTES, INVALID_AUTH);
        const session = readBlob(decoder, MAX_VECTOR_BYTES, INVALID_AUTH);
        if (protocol !== PROTOCOL_V2 || !validGeneration(generation)) return fail(INVALID_AUTH);
        return { type: "auth", token, resume: { protocol: PROTOCOL_V2, generation, chat, session } };
    });
}

export function encodeAuthFrame(frame: AuthFrame): Uint8Array<ArrayBuffer> {
    if (frame.type !== "auth" || !validToken(frame.token) ||
        (frame.resume !== undefined && !validVector(frame.resume))) fail(INVALID_AUTH);
    const encoder = createEncoder();
    writeUint8(encoder, FRAME_AUTH);
    writeUint8(encoder, SYNC_WIRE_VERSION);
    writeText(encoder, frame.token);
    if (frame.resume) {
        writeUint8(encoder, 1);
        writeVarUint(encoder, frame.resume.protocol);
        writeText(encoder, frame.resume.generation);
        writeVarUint8Array(encoder, frame.resume.chat);
        writeVarUint8Array(encoder, frame.resume.session);
    } else {
        writeUint8(encoder, 0);
    }
    const bytes = toUint8Array(encoder);
    if (bytes.byteLength > MAX_AUTH_FRAME_BYTES) fail(FRAME_BUDGET);
    // Every consumer must accept the frame this module produces.
    decodeAuthFrame(bytes);
    return bytes;
}

export function decodeAckFrame(raw: unknown): AckFrame {
    return decodeFrame<AckFrame>(raw, MAX_ACK_FRAME_BYTES, INVALID_ACK, (decoder): AckFrame => {
        if (readUint8(decoder) !== FRAME_ACK || readUint8(decoder) !== SYNC_WIRE_VERSION) return fail(INVALID_ACK);
        const delivery = readCount(decoder, Number.MAX_SAFE_INTEGER, INVALID_ACK);
        if (delivery < 1) return fail(INVALID_ACK);
        return { type: "ack", delivery };
    });
}

export function encodeAckFrame(delivery: number): Uint8Array<ArrayBuffer> {
    if (!isSafeCount(delivery, 1)) fail(INVALID_ACK);
    const encoder = createEncoder();
    writeUint8(encoder, FRAME_ACK);
    writeUint8(encoder, SYNC_WIRE_VERSION);
    writeVarUint(encoder, delivery);
    return toUint8Array(encoder);
}
