export const beginOpen = (commandId, sessionId, previousSessionId, previousChatId) => ({ commandId, sessionId, previousSessionId, previousChatId });
export const matchesOpening = (opening, commandId) => opening?.commandId === commandId;
export const terminalCanCommit = (opening, ack) => matchesOpening(opening, ack.commandId) && (ack.status === 'committed' || ack.status === 'duplicate') && typeof ack.chatId === 'string';
export const shouldClearOpening = (opening, commandId) => matchesOpening(opening, commandId);
export const shouldIgnoreLateAck = (ignoredCommandIds, ack) => ignoredCommandIds.has(ack.commandId) && (ack.status === 'committed' || ack.status === 'duplicate');
