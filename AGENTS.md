# Agent Instructions

- Do not modify files unless the user explicitly approves the specific change.
- Before proposing edits, explain the problem, affected files, and at least one implementation option.
- If a fix is needed, wait for user confirmation before applying patches.
- Analysis, tests, and read-only commands are allowed unless the user says otherwise.
- Do not touch, revert, format, or otherwise modify unrelated files or user changes outside the current task scope.
- If a file has changes you did not intentionally make, do not revert or "clean up" those changes; ask the user before touching it.

## Memory Workflow

Update the `memory/` folder **before the final response** when a user-approved milestone is completed. Common triggers:

- Version bump or changelog update.
- Major feature added, removed, or significantly refactored.
- Public API surface, interface, or schema changed.
- Configuration or environment setup changed (e.g. build tool, deploy target, env vars).
- Global style or asset pipeline changed.
- Any change that would cause the next agent to make wrong assumptions without knowing about it.

### Naming

`memory/YYYY-MM-DD-<version-or-scope>.md` — Keep `memory/README.md` as the index, `memory/TEMPLATE.md` as the required structure.

### Content Rules

- Record facts that help the next agent resume work without rereading the whole conversation.
- Do not store secrets, tokens, private credentials, or irrelevant chat history.
- If validation was skipped or failed, record exactly what was not verified and why.
- Do not edit unrelated memory entries. If an older entry is wrong, create a short correction note or ask the user before rewriting it.
