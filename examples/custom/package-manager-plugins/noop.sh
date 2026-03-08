#!/bin/sh
set -eu

action="${1:-}"

case "$action" in
  prepare)
    # Nothing to prepare for this sample plugin.
    ;;
  auto-install)
    # No dependencies for this sample.
    ;;
  *)
    echo "unknown action: $action" >&2
    exit 2
    ;;
esac
