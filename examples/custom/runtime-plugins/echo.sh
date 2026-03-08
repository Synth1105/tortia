#!/bin/sh
set -eu

action="${1:-}"
[ "$action" = "install" ]

runtime_root="${TORTIA_RUNTIME_ROOT:?}"
bin_file="${TORTIA_RUNTIME_BIN_PATHS_FILE:?}"

mkdir -p "$runtime_root/bin"
cat > "$runtime_root/bin/echo-lang" <<'SCRIPT'
#!/bin/sh
echo "hello from custom runtime plugin"
SCRIPT
chmod +x "$runtime_root/bin/echo-lang"

echo "bin" > "$bin_file"
