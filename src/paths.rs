use std::env;
use std::path::PathBuf;

pub const CONFIG_FILE: &str = "RecipeFile";
pub const MANIFEST_FILE: &str = ".tortia-manifest.toml";
pub const DEFAULT_TOOLS_DIR: &str = ".tortia-tools";
pub const BUILD_TEMP_PREFIX: &str = "tortia-build";
pub const RUN_TEMP_PREFIX: &str = "tortia-run";

const TOOLS_DIR_ENV: &str = "TORTIA_TOOLS_DIR";
const CACHE_DIR_ENV: &str = "TORTIA_CACHE_DIR";

pub fn tools_dir_name() -> String {
    let Some(raw) = env::var_os(TOOLS_DIR_ENV) else {
        return DEFAULT_TOOLS_DIR.to_string();
    };
    let value = raw.to_string_lossy().trim().to_string();
    if value.is_empty() {
        return DEFAULT_TOOLS_DIR.to_string();
    }

    let candidate = PathBuf::from(&value);
    if candidate.is_absolute() {
        return DEFAULT_TOOLS_DIR.to_string();
    }
    if candidate
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return DEFAULT_TOOLS_DIR.to_string();
    }

    value
}

pub fn pm_bin_relative() -> PathBuf {
    PathBuf::from(tools_dir_name()).join("pm").join("bin")
}

pub fn cache_root() -> PathBuf {
    if let Some(path) = env::var_os(CACHE_DIR_ENV) {
        let trimmed = path.to_string_lossy().trim().to_string();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }

    if cfg!(windows) {
        if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local_app_data).join("tortia").join("cache");
        }
    } else {
        if let Some(xdg_cache_home) = env::var_os("XDG_CACHE_HOME") {
            return PathBuf::from(xdg_cache_home).join("tortia");
        }
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home).join(".cache").join("tortia");
        }
    }

    env::temp_dir().join("tortia-cache")
}

pub fn downloads_cache_dir() -> PathBuf {
    cache_root().join("downloads")
}
