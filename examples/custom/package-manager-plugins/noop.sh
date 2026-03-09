#!/bin/sh
set -eu

action="${1:-}"

# No-op: do nothing regardless of action.
# The case statement is removed to prevent errors for unknown actions.
