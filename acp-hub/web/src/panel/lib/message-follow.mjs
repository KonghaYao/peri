export const messageActivity = (entries) => entries.map((entry) =>
  `${entry.id}:${entry.status}:${String(entry.text || '').length}:${(entry.toolCalls || []).map((tool) => tool.status).join(',')}`,
).join('|');

export const nextFollowState = ({ stick, hasNewContent, previousActivity, activity }) => {
  if (stick) return { stick: true, hasNewContent: false, activity };
  return {
    stick: false,
    hasNewContent: hasNewContent || (!!previousActivity && activity !== previousActivity),
    activity,
  };
};
