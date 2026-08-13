export function unimportedSessions(sessions, projectSessions) {
  const imported = new Set(projectSessions.map((session) => session.acpSessionId).filter(Boolean));
  return sessions.filter((candidate) => !imported.has(candidate.sessionId));
}

export function importCandidates(sessions, cwd) {
  return sessions.filter((candidate) => candidate.cwd === cwd);
}
