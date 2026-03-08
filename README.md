# Tortia

`tortia` packages and runs applications in an isolated, non-virtualized execution environment.

It creates a single `.tortia` archive (7z format) that contains:
- your project files
- requested runtimes (for example Node, Rust, Python)
- optional package manager setup
- a run manifest

Then `tortia run` extracts and executes the archive with an isolated `PATH`.

## Features

- `init`, `build`, `run` commands
- Colorful logs (`STEP`, `INFO`, `OK`, `ERROR`)
- Runtime auto-install during build
- Package manager integration and optional auto dependency install
- Host system package manager integration (`brew`, `apt`, `pacman`)
- User extensions (plugin-style event scripts)
- Runtime/command blocking via shims when not requested
- No VM/container runtime required

## Requirements

- macOS or Linux
- `7z`
- `curl`
- `tar`
- POSIX `sh`

Notes:
- Runtime downloads require network access at build time.
- Runtime installation support is currently implemented for macOS/Linux on `x86_64`/`aarch64`.

## Install / Build

```bash
cargo build --release
# binary: target/release/tortia
```

## CLI

```bash
tortia init [dir] [--force]
tortia build [dir] [-o|--output <path>]
tortia run <archive.tortia>
```

## Quick Start

```bash
# 1) Create template config
tortia init .

# 2) Edit RecipeFile

# 3) Build archive

tortia build .

# 4) Run archive
tortia run ./my-app.tortia
```

## Minimal Examples

Each example is intentionally minimal and runnable with only `tortia`.

### Node.js

`index.js`

```js
console.log("hello from node in tortia");
```

`RecipeFile`

```toml
[project]
name = "node-min"

[runtimes]
items = ["node@22.14.0"]

[package_managers]
items = ["npm"]
auto_install = false

[deps]
command = ""

[build]
command = ""

[run]
command = "node index.js"

[bundle]
include = ["index.js"]
exclude = []
```

Build and run:

```bash
tortia build .
tortia run ./node-min.tortia
```

### Python

`main.py`

```python
print("hello from python in tortia")
```

`RecipeFile`

```toml
[project]
name = "python-min"

[runtimes]
items = ["python@3.12"]

[package_managers]
items = ["pip"]
auto_install = false

[deps]
command = ""

[build]
command = ""

[run]
command = "python main.py"

[bundle]
include = ["main.py"]
exclude = []
```

Build and run:

```bash
tortia build .
tortia run ./python-min.tortia
```

### Rust

`Cargo.toml`

```toml
[package]
name = "rust-min"
version = "0.1.0"
edition = "2021"
```

`src/main.rs`

```rust
fn main() {
    println!("hello from rust in tortia");
}
```

`RecipeFile`

```toml
[project]
name = "rust-min"

[runtimes]
items = ["rust@stable"]

[package_managers]
items = ["cargo"]
auto_install = false

[deps]
command = ""

[build]
command = "cargo build --release"

[run]
command = "./target/release/rust-min"

[bundle]
include = ["Cargo.toml", "src"]
exclude = []
```

Build and run:

```bash
tortia build .
tortia run ./rust-min.tortia
```

## RecipeFile

`tortia` reads configuration from `RecipeFile` in the project root.

```toml
[project]
name = "my-app"

[runtimes]
items = ["node@22.14.0", "rust@stable", "python@3.12", "go@1.22.12", "deno@2.2.5", "bun@1.2.5"]

[package_managers]
items = ["npm", "pnpm", "pip", "uv", "cargo", "go", "deno", "bun"]
auto_install = true

[system_packages]
items = ["brew:wget"] # also supports apt:<pkg>, pacman:<pkg>
auto_install = false
use_sudo = false
update = false
missing_only = true

[extensions]
dirs = ["extensions"]
before_deps = []
after_deps = []
before_build = []
after_build = []
before_run = []
after_run = []

[deps]
command = ""

[build]
command = ""

[run]
command = "node app/index.js"

[bundle]
include = ["."]
exclude = [".git", "target"]
```

## Runtime Support

Supported runtimes in `[runtimes].items`:
- `node@<version>` (default: `22.14.0`)
- `rust@<toolchain>` (default: `stable`)
- `python@<version>` or `python`/`python@latest`
- `go@<version>` (default: `1.22.12`)
- `deno@<version>` or `deno`/`deno@latest`
- `bun@<version>` or `bun`/`bun@latest`

## Package Manager Integration

Supported package managers in `[package_managers].items`:
- `npm`, `pnpm`, `yarn`
- `pip`, `uv`
- `cargo`, `go`, `deno`, `bun`

Behavior:
- Required runtimes are auto-added if missing.
  - Example: `package_managers = ["npm"]` auto-adds Node runtime.
- `pnpm`/`yarn` are prepared via `corepack`.
- `pip` and `uv` wrappers are provided inside the isolated environment.

### Auto Dependency Install

If `auto_install = true`, `tortia build` attempts dependency install automatically:

- `npm`: `npm ci` (if `package-lock.json`), else `npm install`
- `pnpm`: `pnpm install --frozen-lockfile` (if lockfile), else `pnpm install`
- `yarn`: `yarn install --immutable || yarn install --frozen-lockfile || yarn install` (if lockfile), else `yarn install`
- `bun`: `bun install` (if `package.json`)
- `uv`: `uv sync` (if `pyproject.toml`) or `uv pip install -r requirements.txt`
- `pip`: `pip install -r requirements.txt` or `pip install .` (if `pyproject.toml`)
- `cargo`: `cargo fetch` (if `Cargo.toml`)
- `go`: `go mod download` (if `go.mod`)
- `deno`: currently only prepares runtime/manager; use explicit `[deps].command` for custom prefetch

## Host System Packages

`tortia` can optionally install host-level system packages during `build`.

Configuration:

```toml
[system_packages]
items = ["brew:cmake", "apt:libssl-dev", "pacman:openssl"]
auto_install = true
use_sudo = true
update = true
missing_only = true
```

Behavior:
- Supported managers: `brew`, `apt`, `pacman`
- Entry format: `<manager>:<package>`
- Install runs on the host (not inside archive)
- `missing_only = true` skips packages already installed
- `use_sudo = true` runs install/update commands with `sudo`

Notes:
- `apt` uses `apt-get`
- `pacman` uses non-interactive flags (`--noconfirm`)
- If a manager is not installed on the host, build fails with an explicit error

## Extensions (Plugins)

`tortia` supports user-defined extension scripts triggered at lifecycle events.

Configuration:

```toml
[extensions]
dirs = [".tortia/extensions", "extensions"]
before_deps = ["prepare-env"]
after_deps = []
before_build = ["gen-assets.sh"]
after_build = []
before_run = ["preflight"]
after_run = ["cleanup"]
```

Resolution rules:
- `dirs` are searched in order
- Script references are relative paths
- If no extension is provided in a reference, `.sh` is also tried

Events:
- `before_deps`, `after_deps`
- `before_build`, `after_build`
- `before_run`, `after_run`

Runtime behavior:
- Build events search scripts from your project root
- Run events search scripts from extracted archive contents
- Scripts execute with:
  - `TORTIA_EVENT`
  - `TORTIA_PROJECT_ROOT`
  - `TORTIA_STAGE_ROOT`

## Isolation Model

`tortia` runs hooks (`[deps]`, `[build]`) and `[run].command` in an isolated environment:

- `PATH` is rebuilt from archive-internal tool paths + system base path
- non-requested runtime commands are blocked by shim executables
- host user-level runtime paths are not used by default

This means a runtime not declared in `RecipeFile` should not be available inside `tortia run`.

## Archive Contents

A `.tortia` archive includes:
- bundled project files from `[bundle].include`
- tool directories (for installed runtimes/package managers)
- `.tortia-manifest.toml` with run metadata

## Troubleshooting

- `could not resolve host ...`
  - Build needs network to download runtimes/tools.
- `command ... is blocked in this isolated environment`
  - Add corresponding runtime in `[runtimes].items`.
- Large archive size
  - Python (Miniconda) can significantly increase archive size.

## Security Note

`[deps].command`, `[build].command`, and `[run].command` execute shell commands. Treat `RecipeFile` as code.
