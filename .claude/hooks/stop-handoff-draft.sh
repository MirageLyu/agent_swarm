#!/usr/bin/env bash
set -euo pipefail

# Stop hook draft handoff generator based on Continuous-Claude / ClawMem patterns.
# Draft-only and fail-open to avoid noisy authoritative handoffs.

ROOT="${CLAUDE_PROJECT_DIR:-$(pwd)}"
HANDOFF_DIR="$ROOT/.claude/handoffs"
mkdir -p "$HANDOFF_DIR"

STAMP="$(date +%Y-%m-%d_%H%M%S)"
OUT="$HANDOFF_DIR/STOP_DRAFT_${STAMP}_handoff.md"

{
  echo "# Stop Draft Handoff"
  echo
  echo "**Date:** $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "**Project:** $ROOT"
  echo "**Status:** draft"
  echo
  echo "## Summary"
  echo
  echo "Session ended. This is an automatically generated draft; verify before using."
  echo
  echo "## Current State"
  echo
  echo "Review git state and recent conversation before treating this as complete."
  echo
  echo "## Git State"
  echo
  if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo '```text'
    git -C "$ROOT" status --short || true
    echo '```'
  else
    echo "Not a git repository."
  fi
  echo
  echo "## Open Work"
  echo
  echo "Unknown from hook context. A human or agent should replace this draft with a structured handoff if needed."
  echo
  echo "## Resume Prompt"
  echo
  echo "Read this draft only as a pointer. Verify repository state and relevant files before acting."
} > "$OUT" || true

printf 'Stop draft handoff: %s\n' "$OUT" >&2
