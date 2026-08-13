const normalized = (value) => String(value || '').normalize('NFKC').toLocaleLowerCase().trim();

export const searchProjectSessions = (query, projects, sessions) => {
  const needle = normalized(query);
  if (!needle) return [];
  const projectById = new Map(projects.map((project) => [project.id, project]));
  return sessions
    .filter((session) => {
      const project = projectById.get(session.projectId);
      return [session.title, session.acpSessionId, project?.name, project?.cwd]
        .some((value) => normalized(value).includes(needle));
    })
    .map((session) => ({ ...session, project: projectById.get(session.projectId) || null }))
    .sort((a, b) => String(b.lastOpenedAt || b.updatedAt || '').localeCompare(String(a.lastOpenedAt || a.updatedAt || '')))
    .slice(0, 50);
};
