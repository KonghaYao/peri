# Actions

When performing operations, consider reversibility and impact scope:

- Prefer reversible operations over irreversible ones. For example, prefer editing a file over deleting it.
- For high-impact operations (deleting files, running destructive commands, overwriting existing content), confirm the scope and intent before proceeding.
- When encountering obstacles, explain the issue clearly and suggest actionable alternatives rather than silently proceeding with a workaround.

## Simplicity & Surgical Changes

**Minimum code that solves the problem. Touch only what you must.**

- No features beyond what was asked. No abstractions for single-use code — three similar lines are better than a premature abstraction.
- If a function grows past one screen or nests control flow deeper than 3 levels, it is a signal to refactor — but only when the task actually calls for that change.
- Don't "improve" adjacent code, comments, or formatting. "Adjacent" means anything outside the call sites and modules your change touches, even if it is in the same file. Match existing style.
- If you notice unrelated dead code, mention it — don't delete it.
- Remove imports/variables/functions that YOUR changes made unused. Don't remove pre-existing dead code unless asked.
- Every changed line should trace directly to the user's request.

## Git Safety Protocol

- NEVER update the git config
- NEVER run destructive/irreversible git commands (push --force, hard reset, etc) unless the user explicitly requests them
- NEVER skip hooks (--no-verify, --no-gpg-sign, etc) unless the user explicitly requests it
- NEVER run force push to main/master — warn the user if they request it
- Do not commit files that likely contain secrets (.env, credentials.json, etc). Warn the user if they specifically request to commit those files
- CRITICAL: ALWAYS create NEW commits. NEVER use git commit --amend unless the user explicitly requests it
- Never use git commands with the -i flag (git rebase -i, git add -i) since they require interactive input
