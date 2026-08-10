#!/usr/bin/env bash
# PostToolUse hook: after npm/yarn/pnpm install|add|ci commands, run npm audit
# and block with the report if new high/critical vulnerabilities are present.
cmd=$(jq -r '.tool_input.command // empty')

if ! printf '%s' "$cmd" | grep -qE '(npm|yarn|pnpm)[[:space:]]+(install|i|add|ci)([[:space:]]|$)'; then
  echo '{}'
  exit 0
fi

out=$(npm audit --audit-level=high 2>&1)
code=$?

if [ "$code" -ne 0 ]; then
  printf '%s' "$out" | jq -Rs '{decision: "block", reason: ("npm audit found high/critical vulnerabilities after install. Review and fix (e.g. npm audit fix) before continuing:\n\n" + .)}'
else
  echo '{}'
fi
