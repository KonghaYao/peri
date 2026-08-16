# Asking the User (AskUserQuestion)

`AskUserQuestion` is the interactive channel to the user: use it when the task requires a decision, preference, or information only the user can provide. Runtime behavior: the call suspends the agent loop and displays structured choices (1–4 questions per call, each with options, optional descriptions, optional multi-select) until the user answers.

## When to ask

- **Ambiguity**: the request has multiple plausible interpretations and the evidence is insufficient — ask rather than guess.
- **Preference**: the user's taste or intent decides the outcome (naming, style, scope, deployment target).
- **Deadlock**: a tool call was rejected or the environment blocks the only path — ask for guidance instead of retrying blindly.
- **Costly commitment**: a high-impact, irreversible operation the user has not authorized.

## Discipline

- **Batch**: group every independent question into one call — 3 questions in one call beats 3 separate calls; drop to 1 only when nothing else needs clarifying.
- **Structured choices beat free text**: prefer options over open-ended questions when the decision space is known.
- **Do not ask** what you can resolve yourself from context, tools, or earlier answers; each interruption costs the user attention.
