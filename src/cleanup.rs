use crate::paths;
use std::env;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct CleanOptions {
    pub project_dir: PathBuf,
    pub temp: bool,
    pub cache: bool,
    pub tools: bool,
    pub dry_run: bool,
}

#[derive(Debug, Default)]
pub struct CleanReport {
    pub removed: Vec<PathBuf>,
    pub missing: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
}

pub fn run_clean(options: &CleanOptions) -> Result<CleanReport, Box<dyn Error>> {
    let mut report = CleanReport::default();
    let project_dir = if options.tools {
        Some(fs::canonicalize(&options.project_dir)?)
    } else {
        None
    };
    let mut targets = Vec::new();

    if options.temp {
        targets.extend(find_temp_dirs(paths::BUILD_TEMP_PREFIX)?);
        targets.extend(find_temp_dirs(paths::RUN_TEMP_PREFIX)?);
    }
    if options.cache {
        targets.push(paths::cache_root());
    }
    if options.tools {
        if let Some(project_dir) = project_dir.as_ref() {
            targets.push(project_dir.join(paths::tools_dir_name()));
        }
    }

    targets.sort();
    targets.dedup();

    for target in targets {
        if !target.exists() {
            report.missing.push(target);
            continue;
        }
        if options.dry_run {
            report.removed.push(target);
            continue;
        }

        if let Err(err) = remove_any(&target) {
            report.failed.push((target, err.to_string()));
        } else {
            report.removed.push(target);
        }
    }

    Ok(report)
}

fn find_temp_dirs(prefix: &str) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut dirs = Vec::new();
    for entry in fs::read_dir(env::temp_dir())? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with(prefix) {
            dirs.push(path);
        }
    }
    Ok(dirs)
}

fn remove_any(path: &Path) -> Result<(), Box<dyn Error>> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}
