#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
python3 - "$repo_root" <<'PYTHON'
"""Guard the known MCP → Host escape hatches; wire regressions verify behavior."""
from pathlib import Path
import sys
import re

root = Path(sys.argv[1])
checks = {
    'peri-acp/src/host/requests/cron.rs': [r'\.list_tasks\(', r'\.toggle\(', r'\.remove\(', r'downcast'],
    'peri-acp/src/session/cron_bridge.rs': [r'scheduler\.subscribe', r'CronSchedulerPort'],
    'peri-agent/src/session/exec/stage_builder/session_setup.rs': [r'scheduler\.subscribe'],
    'peri-acp/src/host/workflow_consumer.rs': [r'Arc<WorkflowRuntime>', r'runtime\.(tasks|run_capacity|subscribe|set_consumer_ready)\('],
    'peri-acp/src/host/workflow_projection.rs': [r'WorkflowRuntime', r'\.registry\(', r'\.bg_registry\('],
    'peri-acp/src/host/workspace.rs': [r'set_bg_registry\(self\.task_manager\(\)\)'],
    'peri-acp/src/host/workspace_workflow.rs': [r'set_bg_registry\(self\.task_manager\(\)\)'],
    'peri-tui/Cargo.toml': [r'^peri-mcp-(cron|workflow)\s*[.=]'],
}
failures = []
for name, patterns in checks.items():
    source = (root / name).read_text()
    for pattern in patterns:
        if re.search(pattern, source, re.M):
            failures.append(f'{name}: forbidden MCP data bypass: {pattern}')
for base in ['peri-acp/src', 'peri-agent/src']:
    for path in (root / base).rglob('*.rs'):
        if 'test' not in path.name and re.search(r'\bWorkflowNotifications\b', path.read_text()):
            failures.append(f'{path.relative_to(root)}: removed broadcast data path returned')
if failures:
    raise SystemExit('\n'.join(failures))
print('MCP data boundary guards passed')

PYTHON
