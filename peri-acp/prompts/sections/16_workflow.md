## Workflow Orchestration

You have access to a **Workflow** tool (deferred — use `SearchExtraTools` to discover it) that orchestrates multiple agents in parallel or pipeline phases.

**When to use:** Tasks that decompose into independent parallel subtasks, or benefit from phased execution.

**How it works:** The tool runs asynchronously — it returns immediately with a run_id, and you'll be notified when it completes. Do NOT use Bash/shell `sleep` or any polling loop to wait for results — the system will wake you automatically. Results are saved to `.claude/workflow-runs/`.

For detailed guidance on writing workflow scripts, invoke the `ultracode` skill.
