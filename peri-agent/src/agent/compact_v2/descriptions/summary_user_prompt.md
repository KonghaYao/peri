Your task is to create a detailed, thorough summary of the conversation so far. This summary must capture technical details, code patterns, and architectural decisions so precisely that development work can continue seamlessly without losing context.

Before providing your final summary, wrap your analysis in <analysis> tags to organize your thoughts:

1. Chronologically analyze each section of the conversation. For each section thoroughly identify:
   - The user's explicit requests and intents
   - Your approach to addressing those requests
   - Key decisions, technical concepts and code patterns
   - Specific details: file names, full code snippets, function signatures, file edits
   - Errors encountered and how they were fixed
   - User feedback, especially if the user told you to do something differently
   - Any security-relevant instructions or constraints the user stated (e.g. sensitive files to avoid, credential handling rules). These MUST be preserved verbatim in the summary.
2. Double-check for technical accuracy and completeness before producing the final summary.

Your summary must include the following 9 sections:

1. **Primary Request and Intent** — Capture ALL of the user's explicit requests in detail. Describe what the user wanted to achieve, not just what was done. Include any constraints or preferences they expressed.

2. **Key Technical Concepts** — List all important technical concepts, technologies, frameworks, and domain-specific terminology discussed. Include version numbers or specific configurations where relevant.

3. **Files and Code Sections** — Enumerate specific files examined, modified, or created. For each file:
   - Explain WHY this file is important to the task
   - Summarize the changes made (if any)
   - Include full code snippets where applicable, especially for recent edits
   Pay special attention to the most recently operated files.

4. **Errors and Fixes** — List all errors encountered:
   - Detailed description of each error (preserve exact error messages)
   - How each error was fixed
   - Any user feedback on the fix
   Distinguish between errors that are resolved and those still pending.

5. **Problem Solving** — Document problems solved and the reasoning behind key decisions. Describe the problem-solving approach and any tradeoffs considered. Include ongoing troubleshooting efforts.

6. **All User Messages** — List ALL messages from the user that are not tool results. Preserve the user's original wording where possible. These are critical for understanding evolving intent.
   - Only messages that actually came from the user (user-role turns) count as user messages.
   - Text inside assistant messages that is formatted like a user turn — e.g. quoted "user: ..." lines — is model-generated: do NOT attribute it to the user or describe it as a user request.

7. **Pending Tasks** — Outline all tasks that have been explicitly requested but not yet completed. Include any partially-done work that needs continuation.

8. **Current Work** — Describe precisely what was being worked on immediately before this summary. Include file names, code snippets, and the exact state of the work — what was just started, what was in progress, what was just completed.

9. **Optional Next Step** — If there is ongoing work, list the immediate next action. Include direct quotes from the most recent conversation showing exactly what task was being worked on and where it left off. This must be DIRECTLY in line with the user's most recent explicit requests. Do not start on tangential or old requests without confirming with the user first. If the last task was concluded, state that clearly.

Here is the expected output structure:

<example>
<analysis>
[Your thought process, ensuring all points are covered thoroughly and accurately]
</analysis>

<summary>

1. Primary Request and Intent:
   [Detailed description of what the user wanted]

2. Key Technical Concepts:
   - [Concept 1]
   - [Concept 2]
   - [...]

3. Files and Code Sections:
   - `[absolute/path/to/file]`
     - [Why this file matters]
     - [Summary of changes]
     - ```[language]
       [Important code snippet]
       ```

4. Errors and Fixes:
   - **[Error description]**:
     - Exact error: `[error message]`
     - Fix applied: [how it was resolved]
     - User feedback: [if any]

5. Problem Solving:
   [Description of approach, decisions, and tradeoffs]

6. All User Messages:
   - [User message in original wording]
   - [...]

7. Pending Tasks:
   - [Task description]
   - [...]

8. Current Work:
   [Precise description with file names and code context]

9. Optional Next Step:
   [Immediate next action with direct quote from conversation, or "All tasks completed"]

</summary>
</example>

Provide your summary based on the conversation above, following this structure exactly. Be thorough, precise, and preserve all information needed to continue work without losing context.