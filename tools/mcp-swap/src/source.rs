//! Resolution and preparation of the five supported server source modes.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::ValueEnum;
use toml_edit::{DocumentMut, Item};

use crate::config::ServerSpec;
use crate::fs::FsError;

/// Where the configured server executable comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum Source {
    /// Build and run `target/debug/<binary>`.
    Debug,
    /// Build and run `target/release/<binary>`.
    Release,
    /// Launch through `cargo run`, rebuilding on startup.
    Run,
    /// Run an explicit executable path.
    Path,
    /// Install and run a version-isolated crates.io release.
    Published,
}

/// Package metadata used to derive default server and binary names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RepoMeta {
    /// Manifest selected for the MCP crate.
    pub manifest: PathBuf,
    /// Package name used by `cargo install`.
    pub package: String,
    /// Default registration key with a trailing `-mcp` removed.
    pub server: String,
    /// Binary produced by Cargo.
    pub binary: String,
}

/// Inputs needed to construct source-specific server definitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceOptions {
    /// Absolute repository root.
    pub repo: PathBuf,
    /// Workspace crate directory or root package name.
    pub crate_name: String,
    /// Cargo binary name.
    pub binary: String,
    /// Version required by [`Source::Published`].
    pub version: Option<String>,
    /// Executable required by [`Source::Path`].
    pub binary_path: Option<PathBuf>,
    /// Private parent for version-isolated published installs.
    pub releases_root: PathBuf,
}

/// Resolve a repository's package, registration, and binary defaults.
///
/// # Errors
///
/// Returns [`FsError`] for a missing or malformed package manifest.
pub fn resolve_repo_meta(repo: &Path, crate_name: &str) -> Result<RepoMeta, FsError> {
    require_path_component(crate_name, "crate name")?;
    let scoped = repo.join("crates").join(crate_name).join("Cargo.toml");
    let manifest = if scoped.is_file() {
        scoped
    } else {
        repo.join("Cargo.toml")
    };
    let text = fs::read_to_string(&manifest)
        .map_err(|error| FsError::new(format!("read {}: {error}", manifest.display())))?;
    let document: DocumentMut = text
        .parse()
        .map_err(|error| FsError::new(format!("parse {}: {error}", manifest.display())))?;
    let package = document
        .get("package")
        .and_then(Item::as_table)
        .ok_or_else(|| FsError::new(format!("{} has no [package] table", manifest.display())))?;
    let name = package
        .get("name")
        .and_then(Item::as_str)
        .ok_or_else(|| FsError::new("package name must be a string"))?
        .to_owned();
    let binary = document
        .get("bin")
        .and_then(Item::as_array_of_tables)
        .and_then(|bins| bins.iter().next())
        .and_then(|binary| binary.get("name"))
        .and_then(Item::as_str)
        .map(str::to_owned)
        .or_else(|| first_source_binary(manifest.parent().unwrap_or(repo)))
        .unwrap_or_else(|| name.clone());
    require_path_component(&name, "package name")?;
    require_path_component(&binary, "binary name")?;
    Ok(RepoMeta {
        manifest,
        package: name.clone(),
        server: name.strip_suffix("-mcp").unwrap_or(&name).to_owned(),
        binary,
    })
}

/// Construct the portable server definition for one source mode.
///
/// # Errors
///
/// Returns [`FsError`] when `path` lacks `--bin` or `published` lacks a
/// version.
pub fn source_spec(source: Source, options: &SourceOptions) -> Result<ServerSpec, FsError> {
    require_path_component(&options.crate_name, "crate name")?;
    require_path_component(&options.binary, "binary name")?;
    let profile = |name: &str| ServerSpec {
        command: options
            .repo
            .join("target")
            .join(name)
            .join(&options.binary)
            .to_string_lossy()
            .into_owned(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    match source {
        Source::Debug => Ok(profile("debug")),
        Source::Release => Ok(profile("release")),
        Source::Run => Ok(ServerSpec {
            command: "cargo".into(),
            args: vec![
                "run".into(),
                "--quiet".into(),
                "--locked".into(),
                "--manifest-path".into(),
                manifest_path(options).to_string_lossy().into_owned(),
                "--bin".into(),
                options.binary.clone(),
                "--".into(),
            ],
            env: BTreeMap::new(),
        }),
        Source::Path => {
            let binary = options
                .binary_path
                .as_ref()
                .ok_or_else(|| FsError::new("--source path needs --bin"))?;
            Ok(ServerSpec {
                command: absolute_path(binary)?.to_string_lossy().into_owned(),
                args: Vec::new(),
                env: BTreeMap::new(),
            })
        }
        Source::Published => {
            let version = options
                .version
                .as_deref()
                .ok_or_else(|| FsError::new("--source published needs --version"))?;
            require_exact_version(version)?;
            Ok(ServerSpec {
                command: published_root(options, version)
                    .join("bin")
                    .join(&options.binary)
                    .to_string_lossy()
                    .into_owned(),
                args: Vec::new(),
                env: BTreeMap::new(),
            })
        }
    }
}

/// Build or install a source when its mode requires preparation.
///
/// # Errors
///
/// Returns [`FsError`] when Cargo fails or the resulting direct executable is
/// absent.
pub fn prepare_source(
    source: Source,
    options: &SourceOptions,
    no_build: bool,
) -> Result<(), FsError> {
    let spec = source_spec(source, options)?;
    match source {
        Source::Debug | Source::Release if !no_build => {
            let mut command = Command::new("cargo");
            command
                .arg("build")
                .arg("--locked")
                .arg("--manifest-path")
                .arg(manifest_path(options))
                .arg("--bin")
                .arg(&options.binary);
            if source == Source::Release {
                command.arg("--release");
            }
            require_success(&mut command, "cargo build")?;
        }
        Source::Published => {
            let version = options
                .version
                .as_deref()
                .ok_or_else(|| FsError::new("--source published needs --version"))?;
            require_exact_version(version)?;
            let root = published_root(options, version);
            let target = root.join("bin").join(&options.binary);
            if !target.is_file() {
                let mut command = Command::new("cargo");
                command
                    .arg("install")
                    .arg(&options.crate_name)
                    .arg("--version")
                    .arg(version)
                    .arg("--root")
                    .arg(&root)
                    .arg("--locked");
                require_success(&mut command, "cargo install")?;
            }
        }
        Source::Debug | Source::Release | Source::Run | Source::Path => {}
    }
    if source != Source::Run && !Path::new(&spec.command).is_file() {
        return Err(FsError::new(format!(
            "{} executable does not exist",
            spec.command
        )));
    }
    Ok(())
}

fn manifest_path(options: &SourceOptions) -> PathBuf {
    let scoped = options
        .repo
        .join("crates")
        .join(&options.crate_name)
        .join("Cargo.toml");
    if scoped.is_file() {
        scoped
    } else {
        options.repo.join("Cargo.toml")
    }
}

fn published_root(options: &SourceOptions, version: &str) -> PathBuf {
    options
        .releases_root
        .join(format!("{}-{version}", options.binary))
}

fn require_path_component(value: &str, label: &str) -> Result<(), FsError> {
    if value.is_empty()
        || matches!(value, "." | "..")
        || value
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'\\' | b'\0'))
    {
        return Err(FsError::new(format!(
            "{label} must be one safe path component"
        )));
    }
    Ok(())
}

fn require_exact_version(version: &str) -> Result<(), FsError> {
    if version.is_empty()
        || matches!(version, "." | "..")
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(FsError::new("--version must be one safe exact version"));
    }
    Ok(())
}

fn absolute_path(path: &Path) -> Result<PathBuf, FsError> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|error| FsError::new(format!("resolve executable path: {error}")))
}

fn first_source_binary(crate_root: &Path) -> Option<String> {
    let entries = fs::read_dir(crate_root.join("src/bin")).ok()?;
    let mut names: Vec<_> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().is_some_and(|extension| extension == "rs"))
                .then(|| path.file_stem()?.to_str().map(str::to_owned))?
        })
        .collect();
    names.sort();
    names.into_iter().next()
}

fn require_success(command: &mut Command, label: &str) -> Result<(), FsError> {
    let status = command
        .status()
        .map_err(|error| FsError::new(format!("launch {label}: {error}")))?;
    if !status.success() {
        return Err(FsError::new(format!("{label} exited with {status}")));
    }
    Ok(())
}
