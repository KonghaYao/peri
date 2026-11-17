---
name: interview
description: "Interview me about my requirements"
userInvocable: true
argumentHint: "[requirements or feature description]"
---

Analyze the requirements given with this invocation (or the user's current request when invoked without arguments) and interview me in detail using the `AskUserQuestion` tool about anything relevant: technical implementation, UI & UX, concerns, tradeoffs, etc. but make sure the questions are not obvious.
Be very in-depth and continue interviewing me continually until it's complete, then draft an implementation plan and get my confirmation before writing code.

Rules:

- Every question MUST have a recommended option: place it first in options, append "(推荐)" to its label, and start its description with the recommendation reason.
- All user-facing text (question, header, label, description) MUST be in Chinese.
