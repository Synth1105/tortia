mod cleanup;
mod paths;

use clap::{Parser, Subcommand};
use cleanup::{CleanOptions, run_clean};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_CONFIG: &str = r#"[project]
name = "my-app"

[runtimes]
items = [] # e.g. ["node@22.14.0", "rust@stable", "python@3.12", "go@1.22.12", "deno@2.2.5", "bun@1.2.5"]

[package_managers]
items = [] # e.g. ["npm", "pnpm", "pip", "uv", "cargo", "go", "deno", "bun"]
auto_install = true

[system_packages]
items = [] # e.g. ["brew:wget", "apt:libssl-dev", "pacman:jq"]
auto_install = false
use_sudo = false
update = false
missing_only = true

[extensions]
dirs = [".tortia/extensions", "extensions"] # searched in order
before_deps = []
after_deps = []
before_build = []
after_build = []
before_run = []
after_run = []

[deps]
command = "" # e.g. npm ci --prefix app

[build]
command = "" # e.g. npm run build --prefix app

[run]
command = "" # e.g. node app/index.js

[bundle]
include = ["."]
exclude = [".git", "target"]
"#;

const COLOR_RESET: &str = "\x1b[0m";
const COLOR_BLUE: &str = "\x1b[34m";
const COLOR_GREEN: &str = "\x1b[32m";
const COLOR_YELLOW: &str = "\x1b[33m";
const COLOR_RED: &str = "\x1b[31m";

#[derive(Parser, Debug)]
#[command(author, version, about = "Package and run apps without virtualization")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    Init {
        #[arg(default_value = ".")]
        dir: PathBuf,
        #[arg(long)]
        force: bool,
    },
    Wrap {
        #[arg(default_value = ".")]
        dir: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    Serve {
        archive: PathBuf,
    },
    Clean {
        #[arg(default_value = ".")]
        dir: PathBuf,
        #[arg(long)]
        temp: bool,
        #[arg(long)]
        cache: bool,
        #[arg(long)]
        tools: bool,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Deserialize)]
struct TortiaConfig {
    project: Option<ProjectConfig>,
    runtimes: Option<RuntimesConfig>,
    package_managers: Option<PackageManagersConfig>,
    system_packages: Option<SystemPackagesConfig>,
    extensions: Option<ExtensionsConfig>,
    deps: Option<HookConfig>,
    build: Option<HookConfig>,
    run: RunConfig,
    bundle: Option<BundleConfig>,
}

#[derive(Debug, Deserialize)]
struct ProjectConfig {
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RuntimesConfig {
    items: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct PackageManagersConfig {
    items: Option<Vec<String>>,
    auto_install: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct SystemPackagesConfig {
    items: Option<Vec<String>>,
    auto_install: Option<bool>,
    use_sudo: Option<bool>,
    update: Option<bool>,
    missing_only: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ExtensionsConfig {
    dirs: Option<Vec<String>>,
    before_deps: Option<Vec<String>>,
    after_deps: Option<Vec<String>>,
    before_build: Option<Vec<String>>,
    after_build: Option<Vec<String>>,
    before_run: Option<Vec<String>>,
    after_run: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct HookConfig {
    command: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RunConfig {
    command: String,
}

#[derive(Debug, Deserialize)]
struct BundleConfig {
    include: Option<Vec<String>>,
    exclude: Option<Vec<String>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct TortiaManifest {
    name: String,
    run_command: String,
    built_at_unix: u64,
    #[serde(default)]
    tool_bin_paths: Vec<String>,
    #[serde(default)]
    extension_dirs: Vec<String>,
    #[serde(default)]
    before_run_extensions: Vec<String>,
    #[serde(default)]
    after_run_extensions: Vec<String>,
}

#[derive(Debug, Clone)]
enum RuntimeSpec {
    Node { version: String },
    Rust { toolchain: String },
    Python { version: Option<String> },
    Go { version: String },
    Deno { version: Option<String> },
    Bun { version: Option<String> },
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
enum RuntimeFamily {
    Node,
    Rust,
    Python,
    Go,
    Deno,
    Bun,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum PackageManagerSpec {
    Npm,
    Pnpm,
    Yarn,
    Pip,
    Uv,
    Cargo,
    Go,
    Deno,
    Bun,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
enum HostPackageManager {
    Brew,
    Apt,
    Pacman,
}

#[derive(Debug, Clone)]
struct HostPackageRequest {
    manager: HostPackageManager,
    package: String,
}

#[derive(Debug, Clone)]
struct ExtensionPlan {
    dirs: Vec<PathBuf>,
    before_deps: Vec<PathBuf>,
    after_deps: Vec<PathBuf>,
    before_build: Vec<PathBuf>,
    after_build: Vec<PathBuf>,
    before_run: Vec<PathBuf>,
    after_run: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
enum ExtensionEvent {
    BeforeDeps,
    AfterDeps,
    BeforeBuild,
    AfterBuild,
    BeforeRun,
    AfterRun,
}

impl RuntimeSpec {
    fn family(&self) -> RuntimeFamily {
        match self {
            RuntimeSpec::Node { .. } => RuntimeFamily::Node,
            RuntimeSpec::Rust { .. } => RuntimeFamily::Rust,
            RuntimeSpec::Python { .. } => RuntimeFamily::Python,
            RuntimeSpec::Go { .. } => RuntimeFamily::Go,
            RuntimeSpec::Deno { .. } => RuntimeFamily::Deno,
            RuntimeSpec::Bun { .. } => RuntimeFamily::Bun,
        }
    }
}

impl PackageManagerSpec {
    fn name(self) -> &'static str {
        match self {
            PackageManagerSpec::Npm => "npm",
            PackageManagerSpec::Pnpm => "pnpm",
            PackageManagerSpec::Yarn => "yarn",
            PackageManagerSpec::Pip => "pip",
            PackageManagerSpec::Uv => "uv",
            PackageManagerSpec::Cargo => "cargo",
            PackageManagerSpec::Go => "go",
            PackageManagerSpec::Deno => "deno",
            PackageManagerSpec::Bun => "bun",
        }
    }

    fn required_runtime(self) -> RuntimeFamily {
        match self {
            PackageManagerSpec::Npm | PackageManagerSpec::Pnpm | PackageManagerSpec::Yarn => {
                RuntimeFamily::Node
            }
            PackageManagerSpec::Pip | PackageManagerSpec::Uv => RuntimeFamily::Python,
            PackageManagerSpec::Cargo => RuntimeFamily::Rust,
            PackageManagerSpec::Go => RuntimeFamily::Go,
            PackageManagerSpec::Deno => RuntimeFamily::Deno,
            PackageManagerSpec::Bun => RuntimeFamily::Bun,
        }
    }
}

impl HostPackageManager {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "brew" | "homebrew" => Some(HostPackageManager::Brew),
            "apt" | "apt-get" => Some(HostPackageManager::Apt),
            "pacman" => Some(HostPackageManager::Pacman),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            HostPackageManager::Brew => "brew",
            HostPackageManager::Apt => "apt",
            HostPackageManager::Pacman => "pacman",
        }
    }
}

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(prefix: &str) -> Result<Self, Box<dyn Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = env::temp_dir().join(format!("{}-{}-{}", prefix, process::id(), stamp));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn main() {
    if let Err(err) = run_cli() {
        log_error(&err.to_string());
        process::exit(1);
    }
}

fn run_cli() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init { dir, force } => init_tortia(&dir, force),
        Commands::Wrap { dir, output } => build_package(&dir, output),
        Commands::Serve { archive } => run_package(&archive),
        Commands::Clean {
            dir,
            temp,
            cache,
            tools,
            all,
            dry_run,
        } => clean_artifacts(&dir, temp, cache, tools, all, dry_run),
    }
}

fn init_tortia(dir: &Path, force: bool) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(dir)?;
    let config_path = dir.join(paths::CONFIG_FILE);

    if config_path.exists() && !force {
        return Err(format!(
            "{} already exists. Use --force to overwrite.",
            config_path.display()
        )
        .into());
    }

    fs::write(&config_path, DEFAULT_CONFIG)?;
    log_success(&format!("Created {}", config_path.display()));
    Ok(())
}

fn clean_artifacts(
    dir: &Path,
    temp: bool,
    cache: bool,
    tools: bool,
    all: bool,
    dry_run: bool,
) -> Result<(), Box<dyn Error>> {
    let mut temp_enabled = temp;
    let mut cache_enabled = cache;
    let mut tools_enabled = tools;

    if all {
        temp_enabled = true;
        cache_enabled = true;
        tools_enabled = true;
    } else if !temp_enabled && !cache_enabled && !tools_enabled {
        temp_enabled = true;
    }

    let options = CleanOptions {
        project_dir: dir.to_path_buf(),
        temp: temp_enabled,
        cache: cache_enabled,
        tools: tools_enabled,
        dry_run,
    };
    let report = run_clean(&options)?;

    if dry_run {
        log_info("Dry-run mode: no files were removed");
    }
    for path in &report.removed {
        if dry_run {
            log_info(&format!("Would remove {}", path.display()));
        } else {
            log_info(&format!("Removed {}", path.display()));
        }
    }
    for path in &report.missing {
        log_info(&format!("Not found {}", path.display()));
    }
    for (path, err) in &report.failed {
        log_error(&format!("Failed to remove {} ({err})", path.display()));
    }

    if report.failed.is_empty() {
        log_success("Clean completed");
        Ok(())
    } else {
        Err("clean completed with failures".into())
    }
}

fn build_package(dir: &Path, output: Option<PathBuf>) -> Result<(), Box<dyn Error>> {
    let project_root = fs::canonicalize(dir)?;
    let config = read_config(&project_root)?;
    let project_name = infer_project_name(&project_root, config.project.as_ref())?;

    if config.run.command.trim().is_empty() {
        return Err("[run].command is required in RecipeFile".into());
    }

    let (include_paths, exclude_paths) = parse_bundle_paths(config.bundle.as_ref())?;
    let package_managers = parse_package_manager_specs(config.package_managers.as_ref())?;
    let mut runtime_specs = parse_runtime_specs(config.runtimes.as_ref())?;
    merge_required_runtimes(&mut runtime_specs, &package_managers);
    let host_packages = parse_host_package_requests(config.system_packages.as_ref())?;
    let extension_plan = parse_extension_plan(config.extensions.as_ref())?;
    let archive_path = resolve_archive_path(&project_root, output, &project_name)?;

    ensure_system_packages_installed(
        &project_root,
        config.system_packages.as_ref(),
        &host_packages,
    )?;

    log_step(&format!("Preparing stage for {project_name}"));
    let temp = TempDir::new(paths::BUILD_TEMP_PREFIX)?;
    let stage_root = temp.path().join("payload");
    fs::create_dir_all(&stage_root)?;

    log_step("Copying bundle files");
    copy_includes(&project_root, &stage_root, &include_paths, &exclude_paths)?;

    log_step("Provisioning isolated runtimes");
    let runtime_bin_paths = ensure_runtimes(&stage_root, &runtime_specs)?;
    prepare_runtime_shims(&stage_root, &runtime_specs)?;
    let mut isolated_env = build_isolated_env(&stage_root, &runtime_bin_paths);

    ensure_package_managers(&stage_root, &package_managers, &isolated_env)?;
    isolated_env = build_isolated_env(&stage_root, &runtime_bin_paths);
    run_extensions(
        &project_root,
        &stage_root,
        &isolated_env,
        &extension_plan,
        ExtensionEvent::BeforeDeps,
    )?;
    auto_install_dependencies(
        &stage_root,
        &package_managers,
        config
            .package_managers
            .as_ref()
            .and_then(|pm| pm.auto_install)
            .unwrap_or(false),
        &isolated_env,
    )?;
    run_extensions(
        &project_root,
        &stage_root,
        &isolated_env,
        &extension_plan,
        ExtensionEvent::AfterDeps,
    )?;

    run_hook(
        &stage_root,
        "deps",
        config.deps.as_ref().and_then(|h| h.command.as_deref()),
        &isolated_env,
    )?;
    run_extensions(
        &project_root,
        &stage_root,
        &isolated_env,
        &extension_plan,
        ExtensionEvent::BeforeBuild,
    )?;
    run_hook(
        &stage_root,
        "build",
        config.build.as_ref().and_then(|h| h.command.as_deref()),
        &isolated_env,
    )?;
    run_extensions(
        &project_root,
        &stage_root,
        &isolated_env,
        &extension_plan,
        ExtensionEvent::AfterBuild,
    )?;

    let manifest = TortiaManifest {
        name: project_name,
        run_command: config.run.command,
        built_at_unix: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
        tool_bin_paths: runtime_bin_paths
            .iter()
            .map(|bin| {
                bin.strip_prefix(&stage_root)
                    .map(|rel| rel.to_string_lossy().to_string())
            })
            .collect::<Result<Vec<_>, _>>()?,
        extension_dirs: extension_plan
            .dirs
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
        before_run_extensions: extension_plan
            .before_run
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
        after_run_extensions: extension_plan
            .after_run
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect(),
    };

    let manifest_toml = toml::to_string_pretty(&manifest)?;
    fs::write(stage_root.join(paths::MANIFEST_FILE), manifest_toml)?;

    if archive_path.exists() {
        fs::remove_file(&archive_path)?;
    }

    log_step(&format!("Creating archive {}", archive_path.display()));
    let status = Command::new("7z")
        .arg("a")
        .arg("-t7z")
        .arg(&archive_path)
        .arg(".")
        .current_dir(&stage_root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if !status.success() {
        return Err("7z failed while creating .tortia archive".into());
    }

    log_success(&format!("Built {}", archive_path.display()));
    Ok(())
}

fn run_package(archive: &Path) -> Result<(), Box<dyn Error>> {
    if !archive.exists() {
        return Err(format!("Archive not found: {}", archive.display()).into());
    }

    let archive = fs::canonicalize(archive)?;
    let temp = TempDir::new(paths::RUN_TEMP_PREFIX)?;

    log_step(&format!("Extracting {}", archive.display()));
    let output_arg = format!("-o{}", temp.path().display());
    let extract_status = Command::new("7z")
        .arg("x")
        .arg(&archive)
        .arg(output_arg)
        .arg("-y")
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if !extract_status.success() {
        return Err("7z failed while extracting .tortia archive".into());
    }

    let manifest_path = temp.path().join(paths::MANIFEST_FILE);
    if !manifest_path.exists() {
        return Err(format!("Invalid .tortia file: missing {}", paths::MANIFEST_FILE).into());
    }

    let manifest: TortiaManifest = toml::from_str(&fs::read_to_string(&manifest_path)?)?;
    if manifest.run_command.trim().is_empty() {
        return Err("Invalid manifest: run_command is empty".into());
    }

    let runtime_bin_paths: Vec<PathBuf> = manifest
        .tool_bin_paths
        .iter()
        .map(|relative| temp.path().join(relative))
        .collect();

    let isolated_env = build_isolated_env(temp.path(), &runtime_bin_paths);
    let run_extension_plan = extension_plan_from_manifest(&manifest)?;
    run_extensions(
        temp.path(),
        temp.path(),
        &isolated_env,
        &run_extension_plan,
        ExtensionEvent::BeforeRun,
    )?;
    log_step(&format!("Running: {}", manifest.run_command));

    let status = run_shell(&manifest.run_command, temp.path(), &isolated_env)?;
    if !status.success() {
        return Err(format!("run command failed: {}", manifest.run_command).into());
    }
    run_extensions(
        temp.path(),
        temp.path(),
        &isolated_env,
        &run_extension_plan,
        ExtensionEvent::AfterRun,
    )?;

    log_success("Run finished successfully");
    Ok(())
}

fn read_config(project_root: &Path) -> Result<TortiaConfig, Box<dyn Error>> {
    let path = project_root.join(paths::CONFIG_FILE);
    let data = fs::read_to_string(&path)
        .map_err(|_| format!("failed to read {}", path.to_string_lossy()))?;
    let config: TortiaConfig = toml::from_str(&data)?;
    Ok(config)
}

fn infer_project_name(
    project_root: &Path,
    project: Option<&ProjectConfig>,
) -> Result<String, Box<dyn Error>> {
    if let Some(project) = project {
        if let Some(name) = &project.name {
            if !name.trim().is_empty() {
                return Ok(name.trim().to_string());
            }
        }
    }

    let fallback = project_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("unable to infer project name from directory")?;

    Ok(fallback.to_string())
}

fn parse_runtime_specs(
    runtimes: Option<&RuntimesConfig>,
) -> Result<Vec<RuntimeSpec>, Box<dyn Error>> {
    let items = runtimes
        .and_then(|r| r.items.as_ref())
        .cloned()
        .unwrap_or_default();

    let mut specs = Vec::new();
    for item in items {
        let normalized = item.trim();
        if normalized.is_empty() {
            continue;
        }

        let (name, version) = normalized
            .split_once('@')
            .map(|(left, right)| (left.trim(), right.trim()))
            .unwrap_or((normalized, ""));

        match name {
            "node" => {
                let version = if version.is_empty() {
                    "22.14.0"
                } else {
                    version.trim_start_matches('v')
                };
                specs.push(RuntimeSpec::Node {
                    version: version.to_string(),
                });
            }
            "rust" => {
                let toolchain = if version.is_empty() {
                    "stable"
                } else {
                    version
                };
                specs.push(RuntimeSpec::Rust {
                    toolchain: toolchain.to_string(),
                });
            }
            "python" | "py" => {
                let resolved = if version.is_empty() || version.eq_ignore_ascii_case("latest") {
                    None
                } else {
                    Some(version.to_string())
                };
                specs.push(RuntimeSpec::Python { version: resolved });
            }
            "go" | "golang" => {
                let resolved = if version.is_empty() {
                    "1.22.12"
                } else {
                    version.trim_start_matches('v')
                };
                specs.push(RuntimeSpec::Go {
                    version: resolved.to_string(),
                });
            }
            "deno" => {
                let resolved = if version.is_empty() || version.eq_ignore_ascii_case("latest") {
                    None
                } else {
                    Some(version.trim_start_matches('v').to_string())
                };
                specs.push(RuntimeSpec::Deno { version: resolved });
            }
            "bun" => {
                let resolved = if version.is_empty() || version.eq_ignore_ascii_case("latest") {
                    None
                } else {
                    Some(version.trim_start_matches('v').to_string())
                };
                specs.push(RuntimeSpec::Bun { version: resolved });
            }
            other => {
                return Err(format!(
                    "unsupported runtime '{other}'. supported: node, rust, python, go, deno, bun (e.g. node@22.14.0, rust@stable, python@3.12, go@1.22.12, deno@2.2.5, bun@1.2.5)"
                )
                .into());
            }
        }
    }

    Ok(specs)
}

fn parse_package_manager_specs(
    package_managers: Option<&PackageManagersConfig>,
) -> Result<Vec<PackageManagerSpec>, Box<dyn Error>> {
    let items = package_managers
        .and_then(|pm| pm.items.as_ref())
        .cloned()
        .unwrap_or_default();

    let mut specs = Vec::new();
    for raw in items {
        let normalized = raw.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            continue;
        }

        let spec = match normalized.as_str() {
            "npm" => PackageManagerSpec::Npm,
            "pnpm" => PackageManagerSpec::Pnpm,
            "yarn" => PackageManagerSpec::Yarn,
            "pip" | "pip3" => PackageManagerSpec::Pip,
            "uv" => PackageManagerSpec::Uv,
            "cargo" => PackageManagerSpec::Cargo,
            "go" | "gomod" | "go-mod" => PackageManagerSpec::Go,
            "deno" => PackageManagerSpec::Deno,
            "bun" => PackageManagerSpec::Bun,
            other => {
                return Err(format!(
                    "unsupported package manager '{other}'. supported: npm, pnpm, yarn, pip, uv, cargo, go, deno, bun"
                )
                .into());
            }
        };

        if !specs.contains(&spec) {
            specs.push(spec);
        }
    }

    Ok(specs)
}

fn parse_host_package_requests(
    system_packages: Option<&SystemPackagesConfig>,
) -> Result<Vec<HostPackageRequest>, Box<dyn Error>> {
    let items = system_packages
        .and_then(|sp| sp.items.as_ref())
        .cloned()
        .unwrap_or_default();

    let mut requests = Vec::new();
    for raw in items {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }

        let (manager_raw, package_raw) = trimmed.split_once(':').ok_or_else(|| {
            format!("invalid system package entry '{trimmed}'. expected '<manager>:<package>'")
        })?;
        let manager_key = manager_raw.trim().to_ascii_lowercase();
        let manager = HostPackageManager::from_name(&manager_key).ok_or_else(|| {
            format!(
                "unsupported system package manager '{manager_raw}'. supported: brew, apt, pacman"
            )
        })?;

        let package = package_raw.trim();
        if package.is_empty() {
            return Err(
                format!("invalid system package entry '{trimmed}': missing package name").into(),
            );
        }
        if !is_safe_package_token(package) {
            return Err(format!(
                "invalid package token '{package}' in '{trimmed}'. only [a-zA-Z0-9._+:/@-] are allowed"
            )
            .into());
        }

        let request = HostPackageRequest {
            manager,
            package: package.to_string(),
        };
        if !requests.iter().any(|existing: &HostPackageRequest| {
            existing.manager == request.manager && existing.package == request.package
        }) {
            requests.push(request);
        }
    }

    Ok(requests)
}

fn parse_extension_plan(
    extensions: Option<&ExtensionsConfig>,
) -> Result<ExtensionPlan, Box<dyn Error>> {
    let default_dirs = vec![".tortia/extensions".to_string(), "extensions".to_string()];
    let dir_values = extensions
        .and_then(|ext| ext.dirs.as_ref())
        .cloned()
        .unwrap_or(default_dirs);

    let dirs = parse_extension_paths(&dir_values, "extensions.dirs")?;

    let before_deps = extensions
        .and_then(|ext| ext.before_deps.clone())
        .unwrap_or_default();
    let after_deps = extensions
        .and_then(|ext| ext.after_deps.clone())
        .unwrap_or_default();
    let before_build = extensions
        .and_then(|ext| ext.before_build.clone())
        .unwrap_or_default();
    let after_build = extensions
        .and_then(|ext| ext.after_build.clone())
        .unwrap_or_default();
    let before_run = extensions
        .and_then(|ext| ext.before_run.clone())
        .unwrap_or_default();
    let after_run = extensions
        .and_then(|ext| ext.after_run.clone())
        .unwrap_or_default();

    Ok(ExtensionPlan {
        dirs,
        before_deps: parse_extension_paths(&before_deps, "extensions.before_deps")?,
        after_deps: parse_extension_paths(&after_deps, "extensions.after_deps")?,
        before_build: parse_extension_paths(&before_build, "extensions.before_build")?,
        after_build: parse_extension_paths(&after_build, "extensions.after_build")?,
        before_run: parse_extension_paths(&before_run, "extensions.before_run")?,
        after_run: parse_extension_paths(&after_run, "extensions.after_run")?,
    })
}

fn extension_plan_from_manifest(
    manifest: &TortiaManifest,
) -> Result<ExtensionPlan, Box<dyn Error>> {
    Ok(ExtensionPlan {
        dirs: parse_extension_paths(&manifest.extension_dirs, "manifest.extension_dirs")?,
        before_deps: Vec::new(),
        after_deps: Vec::new(),
        before_build: Vec::new(),
        after_build: Vec::new(),
        before_run: parse_extension_paths(
            &manifest.before_run_extensions,
            "manifest.before_run_extensions",
        )?,
        after_run: parse_extension_paths(
            &manifest.after_run_extensions,
            "manifest.after_run_extensions",
        )?,
    })
}

fn parse_extension_paths(values: &[String], label: &str) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut paths = Vec::new();
    for raw in values {
        let parsed = parse_bundle_path(raw)?;
        if parsed.as_os_str().is_empty() {
            return Err(format!("{label} cannot contain '.' or empty path").into());
        }
        paths.push(parsed);
    }
    Ok(paths)
}

fn is_safe_package_token(value: &str) -> bool {
    value.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '+' | ':' | '/' | '@' | '-')
    })
}

fn ensure_system_packages_installed(
    project_root: &Path,
    config: Option<&SystemPackagesConfig>,
    requests: &[HostPackageRequest],
) -> Result<(), Box<dyn Error>> {
    if requests.is_empty() {
        return Ok(());
    }

    let enabled = config.and_then(|cfg| cfg.auto_install).unwrap_or(false);
    if !enabled {
        log_info("System packages are configured but auto_install is false. Skipping.");
        return Ok(());
    }

    let use_sudo = config.and_then(|cfg| cfg.use_sudo).unwrap_or(false);
    let update = config.and_then(|cfg| cfg.update).unwrap_or(false);
    let missing_only = config.and_then(|cfg| cfg.missing_only).unwrap_or(true);

    log_step("Installing host system packages");
    let mut updated: HashSet<HostPackageManager> = HashSet::new();

    for request in requests {
        ensure_manager_available(project_root, request.manager)?;
        if missing_only && is_host_package_installed(project_root, request)? {
            log_info(&format!(
                "System package already installed: {}:{}",
                request.manager.name(),
                request.package
            ));
            continue;
        }

        if update && !updated.contains(&request.manager) {
            run_host_package_update(project_root, request.manager, use_sudo)?;
            updated.insert(request.manager);
        }
        run_host_package_install(project_root, request, use_sudo)?;
    }

    Ok(())
}

fn ensure_manager_available(
    project_root: &Path,
    manager: HostPackageManager,
) -> Result<(), Box<dyn Error>> {
    let command = match manager {
        HostPackageManager::Brew => "brew",
        HostPackageManager::Apt => "apt-get",
        HostPackageManager::Pacman => "pacman",
    };
    if command_exists(project_root, command)? {
        return Ok(());
    }
    Err(format!(
        "system package manager '{}' is not available on this host",
        manager.name()
    )
    .into())
}

fn command_exists(project_root: &Path, command: &str) -> Result<bool, Box<dyn Error>> {
    let status = Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {command} >/dev/null 2>&1"))
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

fn is_host_package_installed(
    project_root: &Path,
    request: &HostPackageRequest,
) -> Result<bool, Box<dyn Error>> {
    let (program, args): (&str, Vec<String>) = match request.manager {
        HostPackageManager::Brew => (
            "brew",
            vec![
                "list".to_string(),
                "--versions".to_string(),
                request.package.clone(),
            ],
        ),
        HostPackageManager::Apt => ("dpkg", vec!["-s".to_string(), request.package.clone()]),
        HostPackageManager::Pacman => ("pacman", vec!["-Qi".to_string(), request.package.clone()]),
    };

    let status = Command::new(program)
        .args(args)
        .current_dir(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

fn run_host_package_update(
    project_root: &Path,
    manager: HostPackageManager,
    use_sudo: bool,
) -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = match manager {
        HostPackageManager::Brew => vec!["update".to_string()],
        HostPackageManager::Apt => vec!["update".to_string()],
        HostPackageManager::Pacman => vec!["-Sy".to_string(), "--noconfirm".to_string()],
    };
    log_step(&format!("Updating {} metadata", manager.name()));
    run_host_command(
        project_root,
        manager_binary(manager),
        &args,
        use_sudo,
        "update failed",
    )
}

fn run_host_package_install(
    project_root: &Path,
    request: &HostPackageRequest,
    use_sudo: bool,
) -> Result<(), Box<dyn Error>> {
    log_step(&format!(
        "Installing {}:{}",
        request.manager.name(),
        request.package
    ));

    let mut args = match request.manager {
        HostPackageManager::Brew => vec!["install".to_string()],
        HostPackageManager::Apt => vec!["install".to_string(), "-y".to_string()],
        HostPackageManager::Pacman => vec!["-S".to_string(), "--noconfirm".to_string()],
    };
    args.push(request.package.clone());

    run_host_command(
        project_root,
        manager_binary(request.manager),
        &args,
        use_sudo,
        "install failed",
    )
}

fn manager_binary(manager: HostPackageManager) -> &'static str {
    match manager {
        HostPackageManager::Brew => "brew",
        HostPackageManager::Apt => "apt-get",
        HostPackageManager::Pacman => "pacman",
    }
}

fn run_host_command(
    project_root: &Path,
    program: &str,
    args: &[String],
    use_sudo: bool,
    context: &str,
) -> Result<(), Box<dyn Error>> {
    let mut cmd = if use_sudo {
        let mut command = Command::new("sudo");
        command.arg(program);
        command
    } else {
        Command::new(program)
    };

    let status = cmd
        .args(args)
        .current_dir(project_root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if !status.success() {
        return Err(format!("{program} {context}").into());
    }
    Ok(())
}

fn run_extensions(
    search_root: &Path,
    working_dir: &Path,
    envs: &[(String, String)],
    plan: &ExtensionPlan,
    event: ExtensionEvent,
) -> Result<(), Box<dyn Error>> {
    let items = extension_event_items(plan, event);
    if items.is_empty() {
        return Ok(());
    }

    for item in items {
        let script = resolve_extension_script(search_root, &plan.dirs, item)?;
        log_step(&format!(
            "[extension:{}] {}",
            extension_event_name(event),
            script.display()
        ));

        let mut process = Command::new("sh");
        process
            .arg(&script)
            .current_dir(working_dir)
            .env("TORTIA_EVENT", extension_event_name(event))
            .env("TORTIA_PROJECT_ROOT", search_root)
            .env("TORTIA_STAGE_ROOT", working_dir)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        for (key, value) in envs {
            process.env(key, value);
        }

        let status = process.status()?;
        if !status.success() {
            return Err(format!(
                "extension '{}' failed during event '{}'",
                script.display(),
                extension_event_name(event)
            )
            .into());
        }
    }
    Ok(())
}

fn extension_event_items(plan: &ExtensionPlan, event: ExtensionEvent) -> &[PathBuf] {
    match event {
        ExtensionEvent::BeforeDeps => &plan.before_deps,
        ExtensionEvent::AfterDeps => &plan.after_deps,
        ExtensionEvent::BeforeBuild => &plan.before_build,
        ExtensionEvent::AfterBuild => &plan.after_build,
        ExtensionEvent::BeforeRun => &plan.before_run,
        ExtensionEvent::AfterRun => &plan.after_run,
    }
}

fn extension_event_name(event: ExtensionEvent) -> &'static str {
    match event {
        ExtensionEvent::BeforeDeps => "before_deps",
        ExtensionEvent::AfterDeps => "after_deps",
        ExtensionEvent::BeforeBuild => "before_build",
        ExtensionEvent::AfterBuild => "after_build",
        ExtensionEvent::BeforeRun => "before_run",
        ExtensionEvent::AfterRun => "after_run",
    }
}

fn resolve_extension_script(
    root: &Path,
    dirs: &[PathBuf],
    script_ref: &Path,
) -> Result<PathBuf, Box<dyn Error>> {
    for dir in dirs {
        let base = root.join(dir);
        let candidate = base.join(script_ref);
        if candidate.is_file() {
            return Ok(candidate);
        }
        if candidate.extension().is_none() {
            let with_ext = base.join(format!("{}.sh", script_ref.display()));
            if with_ext.is_file() {
                return Ok(with_ext);
            }
        }
    }

    Err(format!(
        "extension script '{}' was not found in configured extension dirs",
        script_ref.display()
    )
    .into())
}

fn merge_required_runtimes(runtimes: &mut Vec<RuntimeSpec>, managers: &[PackageManagerSpec]) {
    for manager in managers {
        let family = manager.required_runtime();
        if runtimes.iter().any(|runtime| runtime.family() == family) {
            continue;
        }

        let default_runtime = match family {
            RuntimeFamily::Node => RuntimeSpec::Node {
                version: "22.14.0".to_string(),
            },
            RuntimeFamily::Rust => RuntimeSpec::Rust {
                toolchain: "stable".to_string(),
            },
            RuntimeFamily::Python => RuntimeSpec::Python { version: None },
            RuntimeFamily::Go => RuntimeSpec::Go {
                version: "1.22.12".to_string(),
            },
            RuntimeFamily::Deno => RuntimeSpec::Deno { version: None },
            RuntimeFamily::Bun => RuntimeSpec::Bun { version: None },
        };

        log_info(&format!(
            "Auto-adding runtime '{}' for package manager '{}'",
            runtime_family_name(family),
            manager.name()
        ));
        runtimes.push(default_runtime);
    }
}

fn ensure_package_managers(
    stage_root: &Path,
    managers: &[PackageManagerSpec],
    envs: &[(String, String)],
) -> Result<(), Box<dyn Error>> {
    if managers.is_empty() {
        return Ok(());
    }

    log_step("Preparing package managers");
    let pm_bin_root = stage_root.join(paths::pm_bin_relative());
    fs::create_dir_all(&pm_bin_root)?;

    for manager in managers {
        match manager {
            PackageManagerSpec::Npm => {
                ensure_command_available(stage_root, envs, "npm", "runtime 'node' is required")?;
            }
            PackageManagerSpec::Pnpm => {
                ensure_command_available(
                    stage_root,
                    envs,
                    "corepack",
                    "runtime 'node' is required for pnpm",
                )?;
                let prep = run_shell("corepack prepare pnpm@latest --activate", stage_root, envs)?;
                if !prep.success() {
                    return Err("failed to prepare pnpm via corepack".into());
                }
                write_executable_script(
                    &pm_bin_root.join("pnpm"),
                    "#!/bin/sh\nexec corepack pnpm \"$@\"\n",
                )?;
            }
            PackageManagerSpec::Yarn => {
                ensure_command_available(
                    stage_root,
                    envs,
                    "corepack",
                    "runtime 'node' is required for yarn",
                )?;
                let prep = run_shell("corepack prepare yarn@stable --activate", stage_root, envs)?;
                if !prep.success() {
                    return Err("failed to prepare yarn via corepack".into());
                }
                write_executable_script(
                    &pm_bin_root.join("yarn"),
                    "#!/bin/sh\nexec corepack yarn \"$@\"\n",
                )?;
            }
            PackageManagerSpec::Pip => {
                ensure_command_available(
                    stage_root,
                    envs,
                    "python",
                    "runtime 'python' is required for pip",
                )?;
                write_executable_script(
                    &pm_bin_root.join("pip"),
                    "#!/bin/sh\nexec python -m pip \"$@\"\n",
                )?;
                write_executable_script(
                    &pm_bin_root.join("pip3"),
                    "#!/bin/sh\nexec python -m pip \"$@\"\n",
                )?;
            }
            PackageManagerSpec::Uv => {
                ensure_command_available(
                    stage_root,
                    envs,
                    "python",
                    "runtime 'python' is required for uv",
                )?;
                let status = run_shell("python -m pip install --upgrade uv", stage_root, envs)?;
                if !status.success() {
                    return Err("failed to install uv in isolated environment".into());
                }
                write_executable_script(
                    &pm_bin_root.join("uv"),
                    "#!/bin/sh\nexec python -m uv \"$@\"\n",
                )?;
            }
            PackageManagerSpec::Cargo => {
                ensure_command_available(
                    stage_root,
                    envs,
                    "cargo",
                    "runtime 'rust' is required for cargo",
                )?;
            }
            PackageManagerSpec::Go => {
                ensure_command_available(stage_root, envs, "go", "runtime 'go' is required")?;
            }
            PackageManagerSpec::Deno => {
                ensure_command_available(stage_root, envs, "deno", "runtime 'deno' is required")?;
            }
            PackageManagerSpec::Bun => {
                ensure_command_available(stage_root, envs, "bun", "runtime 'bun' is required")?;
            }
        }
    }

    Ok(())
}

fn auto_install_dependencies(
    stage_root: &Path,
    managers: &[PackageManagerSpec],
    enabled: bool,
    envs: &[(String, String)],
) -> Result<(), Box<dyn Error>> {
    if !enabled || managers.is_empty() {
        return Ok(());
    }

    log_step("Auto-installing dependencies");
    let mut js_installed = false;
    let mut py_installed = false;

    for manager in managers {
        match manager {
            PackageManagerSpec::Npm if !js_installed => {
                if stage_root.join("package.json").exists() {
                    let command = if stage_root.join("package-lock.json").exists() {
                        "npm ci"
                    } else {
                        "npm install"
                    };
                    run_required(stage_root, envs, command, "npm auto-install failed")?;
                    js_installed = true;
                }
            }
            PackageManagerSpec::Pnpm if !js_installed => {
                if stage_root.join("package.json").exists() {
                    let command = if stage_root.join("pnpm-lock.yaml").exists() {
                        "pnpm install --frozen-lockfile"
                    } else {
                        "pnpm install"
                    };
                    run_required(stage_root, envs, command, "pnpm auto-install failed")?;
                    js_installed = true;
                }
            }
            PackageManagerSpec::Yarn if !js_installed => {
                if stage_root.join("package.json").exists() {
                    let command = if stage_root.join("yarn.lock").exists() {
                        "yarn install --immutable || yarn install --frozen-lockfile || yarn install"
                    } else {
                        "yarn install"
                    };
                    run_required(stage_root, envs, command, "yarn auto-install failed")?;
                    js_installed = true;
                }
            }
            PackageManagerSpec::Bun if !js_installed => {
                if stage_root.join("package.json").exists() {
                    run_required(stage_root, envs, "bun install", "bun auto-install failed")?;
                    js_installed = true;
                }
            }
            PackageManagerSpec::Uv if !py_installed => {
                if stage_root.join("pyproject.toml").exists() {
                    run_required(stage_root, envs, "uv sync", "uv sync failed")?;
                    py_installed = true;
                } else if stage_root.join("requirements.txt").exists() {
                    run_required(
                        stage_root,
                        envs,
                        "uv pip install -r requirements.txt",
                        "uv requirements install failed",
                    )?;
                    py_installed = true;
                }
            }
            PackageManagerSpec::Pip if !py_installed => {
                if stage_root.join("requirements.txt").exists() {
                    run_required(
                        stage_root,
                        envs,
                        "pip install -r requirements.txt",
                        "pip requirements install failed",
                    )?;
                    py_installed = true;
                } else if stage_root.join("pyproject.toml").exists() {
                    run_required(
                        stage_root,
                        envs,
                        "pip install .",
                        "pip project install failed",
                    )?;
                    py_installed = true;
                }
            }
            PackageManagerSpec::Cargo => {
                if stage_root.join("Cargo.toml").exists() {
                    run_required(stage_root, envs, "cargo fetch", "cargo fetch failed")?;
                }
            }
            PackageManagerSpec::Go => {
                if stage_root.join("go.mod").exists() {
                    run_required(
                        stage_root,
                        envs,
                        "go mod download",
                        "go mod download failed",
                    )?;
                }
            }
            PackageManagerSpec::Deno => {
                if stage_root.join("deno.json").exists() || stage_root.join("deno.jsonc").exists() {
                    log_info(
                        "Deno package manager is prepared. Add explicit deps.command if you need cache prefetch.",
                    );
                }
            }
            _ => {}
        }
    }

    Ok(())
}

fn run_required(
    current_dir: &Path,
    envs: &[(String, String)],
    command: &str,
    fail_message: &str,
) -> Result<(), Box<dyn Error>> {
    log_step(command);
    let status = run_shell(command, current_dir, envs)?;
    if !status.success() {
        return Err(fail_message.to_string().into());
    }
    Ok(())
}

fn ensure_command_available(
    current_dir: &Path,
    envs: &[(String, String)],
    command: &str,
    hint: &str,
) -> Result<(), Box<dyn Error>> {
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(format!("command -v {command} >/dev/null 2>&1"))
        .current_dir(current_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    for (key, value) in envs {
        process.env(key, value);
    }

    let status = process.status()?;
    if !status.success() {
        return Err(
            format!("required command '{command}' not found in isolated PATH ({hint})").into(),
        );
    }
    Ok(())
}

fn write_executable_script(path: &Path, body: &str) -> Result<(), Box<dyn Error>> {
    fs::write(path, body)?;
    let chmod_status = Command::new("/bin/chmod").arg("+x").arg(path).status()?;
    if !chmod_status.success() {
        return Err(format!("failed to set executable permission for {}", path.display()).into());
    }
    Ok(())
}

fn download_with_cache(
    url: &str,
    cache_key: &str,
    destination: &Path,
    working_dir: &Path,
) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }

    let cache_path = paths::downloads_cache_dir().join(cache_key);
    if cache_path.exists() {
        fs::copy(&cache_path, destination)?;
        log_info(&format!("Using cached download {}", cache_path.display()));
        return Ok(());
    }

    log_info(&format!("Downloading {}", url));
    let download_status = Command::new("curl")
        .arg("-fsSL")
        .arg(url)
        .arg("-o")
        .arg(destination)
        .current_dir(working_dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if !download_status.success() {
        return Err(format!("failed to download {url}").into());
    }

    if let Some(parent) = cache_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Err(err) = fs::copy(destination, &cache_path) {
        log_info(&format!(
            "Could not write download cache {} ({err})",
            cache_path.display()
        ));
    }

    Ok(())
}

fn ensure_runtimes(
    stage_root: &Path,
    specs: &[RuntimeSpec],
) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    if specs.is_empty() {
        log_info("No runtimes requested");
        return Ok(Vec::new());
    }

    let tools_root = stage_root.join(paths::tools_dir_name());
    fs::create_dir_all(&tools_root)?;

    let mut bins = Vec::new();
    for spec in specs {
        let bin = match spec {
            RuntimeSpec::Node { version } => install_node_runtime(&tools_root, version)?,
            RuntimeSpec::Rust { toolchain } => install_rust_runtime(&tools_root, toolchain)?,
            RuntimeSpec::Python { version } => {
                install_python_runtime(&tools_root, version.as_deref())?
            }
            RuntimeSpec::Go { version } => install_go_runtime(&tools_root, version)?,
            RuntimeSpec::Deno { version } => install_deno_runtime(&tools_root, version.as_deref())?,
            RuntimeSpec::Bun { version } => install_bun_runtime(&tools_root, version.as_deref())?,
        };

        if !bins.iter().any(|existing: &PathBuf| existing == &bin) {
            bins.push(bin);
        }
    }

    Ok(bins)
}

fn install_node_runtime(tools_root: &Path, version: &str) -> Result<PathBuf, Box<dyn Error>> {
    let os = match env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        other => {
            return Err(format!("node runtime install is not supported on OS: {other}").into());
        }
    };

    let arch = match env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => {
            return Err(format!("node runtime install is not supported on ARCH: {other}").into());
        }
    };

    let folder = format!("node-v{version}-{os}-{arch}");
    let install_dir = tools_root.join(&folder);
    let bin_dir = install_dir.join("bin");

    if bin_dir.join("node").exists() {
        log_info(&format!("runtime node@{version} already installed"));
        return Ok(bin_dir);
    }

    log_step(&format!("Installing runtime node@{version}"));
    let archive_name = format!("{folder}.tar.xz");
    let archive_path = tools_root.join(&archive_name);
    let url = format!("https://nodejs.org/dist/v{version}/{archive_name}");
    download_with_cache(
        &url,
        &format!("node/{archive_name}"),
        &archive_path,
        tools_root,
    )?;

    let extract_status = Command::new("tar")
        .arg("-xJf")
        .arg(&archive_path)
        .arg("-C")
        .arg(tools_root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    let _ = fs::remove_file(&archive_path);

    if !extract_status.success() {
        return Err("failed to extract downloaded node runtime".into());
    }

    if !bin_dir.join("node").exists() {
        return Err("node runtime install completed but node binary was not found".into());
    }

    log_success(&format!("Installed node@{version}"));
    Ok(bin_dir)
}

fn install_rust_runtime(tools_root: &Path, toolchain: &str) -> Result<PathBuf, Box<dyn Error>> {
    let cargo_home = tools_root.join("cargo");
    let rustup_home = tools_root.join("rustup");
    let bin_dir = cargo_home.join("bin");

    fs::create_dir_all(&cargo_home)?;
    fs::create_dir_all(&rustup_home)?;

    if !bin_dir.join("rustup").exists() {
        log_step("Installing runtime rustup");
        let init_script = tools_root.join("rustup-init.sh");
        download_with_cache(
            "https://sh.rustup.rs",
            "scripts/rustup-init.sh",
            &init_script,
            tools_root,
        )?;

        let install_status = Command::new("sh")
            .arg(&init_script)
            .arg("-y")
            .arg("--default-toolchain")
            .arg(toolchain)
            .arg("--profile")
            .arg("minimal")
            .arg("--no-modify-path")
            .current_dir(tools_root)
            .env("CARGO_HOME", &cargo_home)
            .env("RUSTUP_HOME", &rustup_home)
            .env("PATH", base_system_path())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;

        let _ = fs::remove_file(&init_script);

        if !install_status.success() {
            return Err("failed to install rustup for isolated runtime".into());
        }
    }

    log_step(&format!("Ensuring rust toolchain {toolchain}"));
    let status = Command::new(bin_dir.join("rustup"))
        .arg("toolchain")
        .arg("install")
        .arg(toolchain)
        .current_dir(tools_root)
        .env("CARGO_HOME", &cargo_home)
        .env("RUSTUP_HOME", &rustup_home)
        .env(
            "PATH",
            format!("{}:{}", bin_dir.display(), base_system_path()),
        )
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if !status.success() {
        return Err(format!("failed to install rust toolchain: {toolchain}").into());
    }

    let default_status = Command::new(bin_dir.join("rustup"))
        .arg("default")
        .arg(toolchain)
        .current_dir(tools_root)
        .env("CARGO_HOME", &cargo_home)
        .env("RUSTUP_HOME", &rustup_home)
        .env(
            "PATH",
            format!("{}:{}", bin_dir.display(), base_system_path()),
        )
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    if !default_status.success() {
        return Err(format!("failed to set rust default toolchain: {toolchain}").into());
    }

    if !bin_dir.join("rustc").exists() {
        return Err("rust runtime install completed but rustc binary was not found".into());
    }

    log_success(&format!("Installed rust@{toolchain}"));
    Ok(bin_dir)
}

fn install_python_runtime(
    tools_root: &Path,
    version: Option<&str>,
) -> Result<PathBuf, Box<dyn Error>> {
    let platform = match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => "MacOSX-arm64",
        ("macos", "x86_64") => "MacOSX-x86_64",
        ("linux", "aarch64") => "Linux-aarch64",
        ("linux", "x86_64") => "Linux-x86_64",
        (os, arch) => {
            return Err(format!("python runtime install is not supported on {os}/{arch}").into());
        }
    };

    let install_dir = tools_root.join("miniconda");
    let bin_dir = install_dir.join("bin");
    let python_bin = bin_dir.join("python");

    if !python_bin.exists() {
        log_step("Installing runtime python (Miniconda)");
        let installer_path = tools_root.join("miniconda-installer.sh");
        let url = format!("https://repo.anaconda.com/miniconda/Miniconda3-latest-{platform}.sh");
        download_with_cache(
            &url,
            &format!("python/miniconda-{platform}.sh"),
            &installer_path,
            tools_root,
        )?;

        let install_status = Command::new("sh")
            .arg(&installer_path)
            .arg("-b")
            .arg("-p")
            .arg(&install_dir)
            .current_dir(tools_root)
            .env("PATH", base_system_path())
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;

        let _ = fs::remove_file(&installer_path);
        if !install_status.success() {
            return Err("failed to install python runtime (miniconda)".into());
        }
    }

    if let Some(version) = version {
        log_step(&format!("Ensuring python {version}"));
        let status = Command::new(bin_dir.join("conda"))
            .arg("install")
            .arg("-y")
            .arg(format!("python={version}"))
            .current_dir(tools_root)
            .env(
                "PATH",
                format!("{}:{}", bin_dir.display(), base_system_path()),
            )
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()?;

        if !status.success() {
            return Err(format!("failed to install python version {version}").into());
        }
    }

    if !python_bin.exists() {
        return Err("python runtime install completed but python binary was not found".into());
    }

    log_success("Installed python runtime");
    Ok(bin_dir)
}

fn install_go_runtime(tools_root: &Path, version: &str) -> Result<PathBuf, Box<dyn Error>> {
    let (os, arch) = match (env::consts::OS, env::consts::ARCH) {
        ("macos", "aarch64") => ("darwin", "arm64"),
        ("macos", "x86_64") => ("darwin", "amd64"),
        ("linux", "aarch64") => ("linux", "arm64"),
        ("linux", "x86_64") => ("linux", "amd64"),
        (os, arch) => {
            return Err(format!("go runtime install is not supported on {os}/{arch}").into());
        }
    };

    let install_dir = tools_root.join(format!("go-{version}-{os}-{arch}"));
    let bin_dir = install_dir.join("go").join("bin");
    if bin_dir.join("go").exists() {
        log_info(&format!("runtime go@{version} already installed"));
        return Ok(bin_dir);
    }

    log_step(&format!("Installing runtime go@{version}"));
    fs::create_dir_all(&install_dir)?;
    let archive_name = format!("go{version}.{os}-{arch}.tar.gz");
    let archive_path = tools_root.join(&archive_name);
    let url = format!("https://go.dev/dl/{archive_name}");
    download_with_cache(
        &url,
        &format!("go/{archive_name}"),
        &archive_path,
        tools_root,
    )?;

    let extract_status = Command::new("tar")
        .arg("-xzf")
        .arg(&archive_path)
        .arg("-C")
        .arg(&install_dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;

    let _ = fs::remove_file(&archive_path);
    if !extract_status.success() {
        return Err("failed to extract downloaded go runtime".into());
    }

    if !bin_dir.join("go").exists() {
        return Err("go runtime install completed but go binary was not found".into());
    }

    log_success(&format!("Installed go@{version}"));
    Ok(bin_dir)
}

fn install_deno_runtime(
    tools_root: &Path,
    version: Option<&str>,
) -> Result<PathBuf, Box<dyn Error>> {
    let install_dir = tools_root.join("deno");
    let bin_dir = install_dir.join("bin");
    let deno_bin = bin_dir.join("deno");

    if deno_bin.exists() && version.is_none() {
        log_info("runtime deno already installed");
        return Ok(bin_dir);
    }

    let label = version
        .map(|v| format!("deno@{v}"))
        .unwrap_or_else(|| "deno@latest".to_string());
    log_step(&format!("Installing runtime {label}"));

    fs::create_dir_all(&install_dir)?;
    let script_path = tools_root.join("deno-install.sh");
    download_with_cache(
        "https://deno.land/install.sh",
        "scripts/deno-install.sh",
        &script_path,
        tools_root,
    )?;

    let mut install_cmd = Command::new("sh");
    install_cmd
        .arg(&script_path)
        .current_dir(tools_root)
        .env("DENO_INSTALL", &install_dir)
        .env("PATH", base_system_path())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Some(version) = version {
        install_cmd.arg(format!("v{version}"));
    }

    let install_status = install_cmd.status()?;
    let _ = fs::remove_file(&script_path);
    if !install_status.success() {
        return Err(format!("failed to install runtime {label}").into());
    }

    if !deno_bin.exists() {
        return Err("deno runtime install completed but deno binary was not found".into());
    }

    log_success(&format!("Installed {label}"));
    Ok(bin_dir)
}

fn install_bun_runtime(
    tools_root: &Path,
    version: Option<&str>,
) -> Result<PathBuf, Box<dyn Error>> {
    let install_dir = tools_root.join("bun");
    let bin_dir = install_dir.join("bin");
    let bun_bin = bin_dir.join("bun");

    if bun_bin.exists() && version.is_none() {
        log_info("runtime bun already installed");
        return Ok(bin_dir);
    }

    let label = version
        .map(|v| format!("bun@{v}"))
        .unwrap_or_else(|| "bun@latest".to_string());
    log_step(&format!("Installing runtime {label}"));

    fs::create_dir_all(&install_dir)?;
    let script_path = tools_root.join("bun-install.sh");
    download_with_cache(
        "https://bun.sh/install",
        "scripts/bun-install.sh",
        &script_path,
        tools_root,
    )?;

    let mut install_cmd = Command::new("sh");
    install_cmd
        .arg(&script_path)
        .current_dir(tools_root)
        .env("BUN_INSTALL", &install_dir)
        .env("PATH", base_system_path())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    if let Some(version) = version {
        install_cmd.arg(format!("bun-v{version}"));
    }

    let install_status = install_cmd.status()?;
    let _ = fs::remove_file(&script_path);
    if !install_status.success() {
        return Err(format!("failed to install runtime {label}").into());
    }

    if !bun_bin.exists() {
        return Err("bun runtime install completed but bun binary was not found".into());
    }

    log_success(&format!("Installed {label}"));
    Ok(bin_dir)
}

fn prepare_runtime_shims(stage_root: &Path, specs: &[RuntimeSpec]) -> Result<(), Box<dyn Error>> {
    let requested: HashSet<RuntimeFamily> = specs.iter().map(RuntimeSpec::family).collect();
    let shims_root = stage_root.join(paths::tools_dir_name());
    fs::create_dir_all(&shims_root)?;

    let runtime_commands: &[(RuntimeFamily, &[&str])] = &[
        (
            RuntimeFamily::Node,
            &["node", "npm", "npx", "corepack", "pnpm", "yarn"],
        ),
        (
            RuntimeFamily::Rust,
            &[
                "rustc",
                "cargo",
                "rustup",
                "rustfmt",
                "clippy-driver",
                "cargo-clippy",
            ],
        ),
        (
            RuntimeFamily::Python,
            &["python", "python3", "pip", "pip3", "uv"],
        ),
        (RuntimeFamily::Go, &["go", "gofmt"]),
        (RuntimeFamily::Deno, &["deno"]),
        (RuntimeFamily::Bun, &["bun", "bunx"]),
    ];

    for (family, commands) in runtime_commands {
        if requested.contains(family) {
            continue;
        }

        let family_name = runtime_family_name(*family);
        for command in *commands {
            let shim_path = shims_root.join(command);
            let body = format!(
                "#!/bin/sh\necho \"tortia: '{command}' is blocked in this isolated environment (add runtime '{family_name}' in [runtimes].items)\" >&2\nexit 127\n"
            );
            write_executable_script(&shim_path, &body)?;
        }
    }

    Ok(())
}

fn runtime_family_name(family: RuntimeFamily) -> &'static str {
    match family {
        RuntimeFamily::Node => "node",
        RuntimeFamily::Rust => "rust",
        RuntimeFamily::Python => "python",
        RuntimeFamily::Go => "go",
        RuntimeFamily::Deno => "deno",
        RuntimeFamily::Bun => "bun",
    }
}

fn build_isolated_env(payload_root: &Path, runtime_bin_paths: &[PathBuf]) -> Vec<(String, String)> {
    let mut path_entries: Vec<String> = Vec::new();

    let shims_dir = payload_root.join(paths::tools_dir_name());
    if shims_dir.exists() {
        path_entries.push(shims_dir.to_string_lossy().to_string());
    }

    let pm_bin_dir = payload_root.join(paths::pm_bin_relative());
    if pm_bin_dir.exists() {
        path_entries.push(pm_bin_dir.to_string_lossy().to_string());
    }

    path_entries.extend(
        runtime_bin_paths
            .iter()
            .filter(|path| path.exists())
            .map(|path| path.to_string_lossy().to_string()),
    );

    path_entries.push(base_system_path());

    let mut envs = vec![("PATH".to_string(), path_entries.join(":"))];

    let cargo_home = payload_root.join(paths::tools_dir_name()).join("cargo");
    if cargo_home.exists() {
        envs.push((
            "CARGO_HOME".to_string(),
            cargo_home.to_string_lossy().to_string(),
        ));
    }

    let rustup_home = payload_root.join(paths::tools_dir_name()).join("rustup");
    if rustup_home.exists() {
        envs.push((
            "RUSTUP_HOME".to_string(),
            rustup_home.to_string_lossy().to_string(),
        ));
    }

    envs
}

fn base_system_path() -> String {
    "/usr/bin:/bin:/usr/sbin:/sbin".to_string()
}

fn resolve_archive_path(
    project_root: &Path,
    output: Option<PathBuf>,
    project_name: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    let path = match output {
        Some(path) if path.is_absolute() => path,
        Some(path) => project_root.join(path),
        None => project_root.join(format!("{}.tortia", project_name)),
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    Ok(path)
}

fn run_hook(
    stage_root: &Path,
    label: &str,
    command: Option<&str>,
    envs: &[(String, String)],
) -> Result<(), Box<dyn Error>> {
    let Some(command) = command else {
        return Ok(());
    };

    let command = command.trim();
    if command.is_empty() {
        return Ok(());
    }

    log_step(&format!("[{label}] {command}"));
    let status = run_shell(command, stage_root, envs)?;

    if !status.success() {
        return Err(format!("{label} hook failed: {command}").into());
    }

    Ok(())
}

fn run_shell(
    command: &str,
    current_dir: &Path,
    envs: &[(String, String)],
) -> Result<std::process::ExitStatus, Box<dyn Error>> {
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(command)
        .current_dir(current_dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    for (key, value) in envs {
        process.env(key, value);
    }

    Ok(process.status()?)
}

fn parse_bundle_paths(
    bundle: Option<&BundleConfig>,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), Box<dyn Error>> {
    let include_values = bundle
        .and_then(|b| b.include.as_ref())
        .cloned()
        .unwrap_or_else(|| vec![".".to_string()]);
    let exclude_values = bundle
        .and_then(|b| b.exclude.as_ref())
        .cloned()
        .unwrap_or_default();

    let mut includes = Vec::new();
    for raw in include_values {
        includes.push(parse_bundle_path(&raw)?);
    }

    if includes.is_empty() {
        includes.push(PathBuf::new());
    }

    let mut excludes = Vec::new();
    for raw in exclude_values {
        let parsed = parse_bundle_path(&raw)?;
        if !parsed.as_os_str().is_empty() {
            excludes.push(parsed);
        }
    }

    Ok((includes, excludes))
}

fn parse_bundle_path(raw: &str) -> Result<PathBuf, Box<dyn Error>> {
    let path = Path::new(raw);
    if path.as_os_str().is_empty() || raw.trim() == "." {
        return Ok(PathBuf::new());
    }

    if path.is_absolute() {
        return Err(format!("bundle path must be relative: {raw}").into());
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                return Err(format!("bundle path cannot contain '..': {raw}").into());
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!("bundle path must be relative: {raw}").into());
            }
        }
    }

    Ok(normalized)
}

fn copy_includes(
    project_root: &Path,
    stage_root: &Path,
    includes: &[PathBuf],
    excludes: &[PathBuf],
) -> Result<(), Box<dyn Error>> {
    for include in includes {
        if include.as_os_str().is_empty() {
            copy_dir_contents(project_root, stage_root, project_root, excludes)?;
            continue;
        }

        let source = project_root.join(include);
        if !source.exists() {
            return Err(format!("bundle.include path not found: {}", include.display()).into());
        }

        if source.is_file() {
            if !is_excluded(include, excludes) {
                let dest = stage_root.join(include);
                copy_file_with_parents(&source, &dest)?;
            }
            continue;
        }

        copy_dir_recursive(&source, stage_root, project_root, excludes)?;
    }

    Ok(())
}

fn copy_dir_recursive(
    source_dir: &Path,
    stage_root: &Path,
    project_root: &Path,
    excludes: &[PathBuf],
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(source_dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;

        if metadata.file_type().is_symlink() {
            return Err(format!("symlink is not supported in bundle: {}", path.display()).into());
        }

        let rel = path.strip_prefix(project_root)?;
        if is_excluded(rel, excludes) {
            continue;
        }

        if metadata.is_dir() {
            fs::create_dir_all(stage_root.join(rel))?;
            copy_dir_recursive(&path, stage_root, project_root, excludes)?;
        } else {
            copy_file_with_parents(&path, &stage_root.join(rel))?;
        }
    }

    Ok(())
}

fn copy_dir_contents(
    source_dir: &Path,
    stage_root: &Path,
    project_root: &Path,
    excludes: &[PathBuf],
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(source_dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;

        if metadata.file_type().is_symlink() {
            return Err(format!("symlink is not supported in bundle: {}", path.display()).into());
        }

        let rel = path.strip_prefix(project_root)?;
        if is_excluded(rel, excludes) {
            continue;
        }

        if metadata.is_dir() {
            fs::create_dir_all(stage_root.join(rel))?;
            copy_dir_recursive(&path, stage_root, project_root, excludes)?;
        } else {
            copy_file_with_parents(&path, &stage_root.join(rel))?;
        }
    }

    Ok(())
}

fn copy_file_with_parents(source: &Path, destination: &Path) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, destination)?;
    Ok(())
}

fn is_excluded(relative_path: &Path, excludes: &[PathBuf]) -> bool {
    excludes
        .iter()
        .any(|exclude| relative_path.starts_with(exclude))
}

fn supports_color() -> bool {
    env::var_os("NO_COLOR").is_none()
}

fn log_with(level: &str, color: &str, message: &str) {
    if supports_color() {
        println!("{color}[{level}]{COLOR_RESET} {message}");
    } else {
        println!("[{level}] {message}");
    }
}

fn log_step(message: &str) {
    log_with("STEP", COLOR_BLUE, message);
}

fn log_info(message: &str) {
    log_with("INFO", COLOR_YELLOW, message);
}

fn log_success(message: &str) {
    log_with("OK", COLOR_GREEN, message);
}

fn log_error(message: &str) {
    if supports_color() {
        eprintln!("{COLOR_RED}[ERROR]{COLOR_RESET} {message}");
    } else {
        eprintln!("[ERROR] {message}");
    }
}
