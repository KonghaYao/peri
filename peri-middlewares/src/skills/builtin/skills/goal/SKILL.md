---
name: goal
description: >
  Long-running goal tracking. Use when the user gives a complex task requiring
  multiple steps to complete.
  Triggers: "goal", "keep going until done", "don't stop until X".
userInvocable: true
argumentHint: "[objective description]"
---

# Goal Mode

## When to Use

- The user gives a complex task that requires multiple rounds to complete
- The user says "keep going until done", "don't stop midway", "until X"

## How to Use

1. Call the `goal` tool with action=create and a specific, verifiable objective
2. Work continuously until the goal is achieved
3. Achieved → `goal` tool, action=complete
4. Blocked by something unsolvable → `goal` tool, action=block, reason=why you're blocked
5. Check current goal status anytime with action=get

## Important Constraints

- **Objectives must be specific and verifiable**: "improve code" is bad, "raise test coverage to 80%" is good
- **complete is verified**: The system uses an auxiliary LLM to check if you actually achieved the goal. If verification fails, you'll get the reason back
- **block is a distress signal**: Only use when truly unable to continue (missing permissions, missing dependencies)
- **Self-driven after creation**: Once a goal is created, you'll receive a reminder after each turn. You must decide: continue / complete / block
- **Singleton**: Only one goal at a time. Clear the existing goal before creating a new one
