export { SessionDocSync } from "./session-doc-sync";
export { SessionDocReplica } from "./session-doc-replica";
export { SyncBackpressureError } from "./peer";
export type { DocStateVector, DocSnapshot, DocUpdate, DocSyncOptions } from "./session-doc-sync";
export { decodeAckFrame, decodeAuthFrame, decodeSyncFrame, encodeAckFrame, encodeAuthFrame, encodeSyncFrame,
  frameBytes, FRAME_ACK, FRAME_AUTH, FRAME_SNAPSHOT, FRAME_UPDATE, MAX_ACK_FRAME_BYTES, MAX_AUTH_FRAME_BYTES,
  MAX_SYNC_FRAME_BYTES, SYNC_WIRE_VERSION } from "./wire";
export type { AckFrame, AuthFrame, SyncFrame, SyncFramePayload } from "./wire";
export { acknowledgeDelivery, credentialFingerprint, reserveDeliveryFrame } from "./delivery";
export type { DeliveryCreditPolicy, DeliveryLedger, DeliveryReservation } from "./delivery";
