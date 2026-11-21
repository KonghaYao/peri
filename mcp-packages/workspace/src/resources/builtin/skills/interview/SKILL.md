---
name: interview
description: "Interview me about any topic before planning"
userInvocable: true
argumentHint: "[any topic, idea, requirement, or question]"
---

Analyze the topic given with this invocation (or the user's current request when invoked without arguments) and interview me in detail using the `AskUserQuestion` tool about anything relevant: goals, technical implementation, UI & UX, constraints, concerns, tradeoffs, etc. but make sure the questions are not obvious.
The topic can be anything — a feature, a bug, a product idea, a research question, a design decision, or a writing task — not only software requirements.
Be very in-depth and continue interviewing me continually until it's complete, then draft an implementation plan (or a concrete action plan for non-code topics) and get my confirmation before writing code.

Rules:

- Every question MUST have a recommended option: place it first in options, append "(推荐)" to its label, and start its description with the recommendation reason.
- All user-facing text (question, header, label, description) MUST be in Chinese.
