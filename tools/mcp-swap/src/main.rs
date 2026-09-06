//! Point supported agent clients at one tmux MCP build and restore them later.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};
use mcp_swap::catalog::{Client, Paths, known_clients, select_clients};
use mcp_swap::config::{Scope, read_server};
use mcp_swap::fs::{FsError, resolve_config_route};
use mcp_swap::preflight::preflight;
use mcp_swap::recovery::load_ledger;
use mcp_swap::source::{Source, SourceOptions, prepare_source, resolve_repo_meta, source_spec};
use mcp_swap::transaction::{
    RevertRequest, UseRequest, planned_use_specs, revert_clients, use_clients,
    use_clients_preflighted,
};

#[derive(Debug, Parser)]
#[command(
    name = "mcp-swap",
    about = "Swap tmux MCP configs across supported agent clients"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List client binaries and global configuration files.
    Detect,
    /// Show the current server entry for each selected client.
    Status(StatusArgs),
    /// Rewrite selected configurations to run one server source.
    Use(UseArgs),
    /// Restore selected configurations from their first backups.
    Revert(RevertArgs),
    /// Report configuration, recovery, and authentication diagnostics.
    Doctor(DoctorArgs),
}

#[derive(Debug, Args)]
struct Selection {
    /// Limit to repeatable or comma-separated client names: claude, codex,
    /// cursor, gemini, grok, agy (or antigravity), opencode, pi.
    #[arg(long, value_name = "NAME", value_delimiter = ',', action = clap::ArgAction::Append)]
    cli: Vec<String>,
}

#[derive(Debug, Args)]
struct StatusArgs {
    /// Repository root used to locate Claude's project layer.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// MCP registration key. Defaults to the package name without `-mcp`.
    #[arg(long)]
    server: Option<String>,
    /// Restrict Claude to one layer. Other clients are user-scoped.
    #[arg(long, value_enum)]
    scope: Option<Scope>,
    #[command(flatten)]
    selection: Selection,
}

#[derive(Debug, Args)]
struct UseArgs {
    /// Repository root containing the MCP crate.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// Server source: debug, release, run, path, or published.
    #[arg(long, value_enum, default_value = "debug")]
    source: Source,
    /// crates.io version required by `--source published`.
    #[arg(long)]
    version: Option<String>,
    /// Executable required by `--source path`.
    #[arg(long = "bin")]
    binary_path: Option<PathBuf>,
    /// Workspace crate providing the MCP server.
    #[arg(long = "crate", default_value = "tmux-mcp")]
    crate_name: String,
    /// Skip the debug or release build when its executable is already current.
    #[arg(long)]
    no_build: bool,
    /// Skip the MCP initialize handshake before writing.
    #[arg(long)]
    no_preflight: bool,
    /// MCP registration key. Defaults to the package name without `-mcp`.
    #[arg(long)]
    server: Option<String>,
    /// Cargo binary name. Defaults to package metadata.
    #[arg(long)]
    entry: Option<String>,
    /// Environment entry in `KEY=VALUE` form. Repeatable.
    #[arg(long, value_parser = parse_environment, action = clap::ArgAction::Append)]
    env: Vec<(String, String)>,
    /// Claude layer. Defaults to project; other clients normalize to user.
    #[arg(long, value_enum, default_value = "project")]
    scope: Scope,
    /// Validate and print the plan without building, launching, or writing.
    #[arg(long)]
    dry_run: bool,
    #[command(flatten)]
    selection: Selection,
}

#[derive(Debug, Args)]
struct RevertArgs {
    /// Restrict Claude restoration to one layer.
    #[arg(long, value_enum)]
    scope: Option<Scope>,
    /// Validate and print the plan without writing.
    #[arg(long)]
    dry_run: bool,
    #[command(flatten)]
    selection: Selection,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    /// Repository root used for source descriptions.
    #[arg(long, default_value = ".")]
    repo: PathBuf,
    /// MCP registration key. Defaults to the package name without `-mcp`.
    #[arg(long)]
    server: Option<String>,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), FsError> {
    let paths = Paths::discover().map_err(|error| FsError::new(error.to_string()))?;
    let clients = known_clients(&paths);
    match cli.command {
        Command::Detect => {
            detect(&paths, &clients);
            Ok(())
        }
        Command::Status(args) => status(&clients, args),
        Command::Use(args) => use_command(&paths, &clients, args),
        Command::Revert(args) => revert_command(&paths, &clients, &args),
        Command::Doctor(args) => doctor(&paths, &clients, args),
    }
}

fn detect(paths: &Paths, clients: &[Client]) {
    for client in clients {
        let binary = binary_on_path(client.binary);
        let config = client.config_path.exists();
        let present = if binary && config { "yes" } else { " no" };
        let mut details = Vec::new();
        if !binary {
            details.push("binary missing".to_owned());
        }
        if !config {
            details.push(format!("config missing: {}", client.config_path.display()));
        }
        if client.name.as_str() == "pi"
            && !paths
                .home
                .join(".pi/agent/npm/node_modules/pi-mcp-adapter")
                .is_dir()
        {
            details.push("needs the pi-mcp-adapter package; pi has no built-in MCP client".into());
        }
        let suffix = if details.is_empty() {
            String::new()
        } else {
            format!("  ({})", details.join(", "))
        };
        println!("  [{present}] {:<9}{suffix}", client.name.as_str());
    }
}

fn status(clients: &[Client], args: StatusArgs) -> Result<(), FsError> {
    let repo = absolute_existing(&args.repo, "repository")?;
    let metadata = resolve_repo_meta(&repo, "tmux-mcp")?;
    let server = args.server.unwrap_or(metadata.server);
    let selected = selected_for_read(clients, &args.selection.cli)?;
    for client in selected {
        if !client.config_path.exists() {
            println!("[{}] no config", client.name.as_str());
            continue;
        }
        let bytes = match fs::read(&client.config_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("[{}] {error}", client.name.as_str());
                continue;
            }
        };
        let scopes: &[Scope] = if client.name.as_str() == "claude" {
            match args.scope {
                Some(Scope::User) => &[Scope::User],
                Some(Scope::Project) => &[Scope::Project],
                None => &[Scope::User, Scope::Project],
            }
        } else {
            &[Scope::User]
        };
        for scope in scopes {
            let label = if client.name.as_str() == "claude" {
                format!("claude:{}", scope_name(*scope))
            } else {
                client.name.as_str().into()
            };
            match read_server(&client.config, &bytes, &server, &repo, *scope) {
                Ok(Some(spec)) => println!(
                    "[{label}] {server} = {} {} env={:?}",
                    spec.command,
                    spec.args.join(" "),
                    spec.env.keys().collect::<Vec<_>>()
                ),
                Ok(None) => println!("[{label}] no entry for {server:?}"),
                Err(error) => eprintln!("[{label}] {error}"),
            }
        }
    }
    Ok(())
}

fn use_command(paths: &Paths, clients: &[Client], args: UseArgs) -> Result<(), FsError> {
    let repo = absolute_existing(&args.repo, "repository")?;
    let metadata = resolve_repo_meta(&repo, &args.crate_name)?;
    let options = SourceOptions {
        repo: repo.clone(),
        crate_name: args.crate_name,
        binary: args.entry.unwrap_or(metadata.binary),
        version: args.version,
        binary_path: args.binary_path,
        releases_root: paths.state_home.join("libtmux-mcp-dev/releases/rust"),
    };
    let mut spec = source_spec(args.source, &options)?;
    spec.env = args.env.into_iter().collect::<BTreeMap<_, _>>();
    let server = args.server.unwrap_or(metadata.server);
    let selected = selected_for_use(clients, &args.selection.cli)?;
    let request = UseRequest {
        repo,
        server,
        scope: args.scope,
        spec: spec.clone(),
    };
    if args.dry_run {
        for change in use_clients(paths, &selected, &request, true)? {
            println!(
                "[{}:{}] would {:?}",
                change.client,
                scope_name(change.scope),
                change.action
            );
        }
        return Ok(());
    }
    let planned_specs = planned_use_specs(paths, &selected, &request)?;
    prepare_source(args.source, &options, args.no_build)?;
    if !args.no_preflight {
        let mut checked = Vec::new();
        for planned in &planned_specs {
            if checked.contains(planned) {
                continue;
            }
            eprintln!("preflight: {} {}", planned.command, planned.args.join(" "));
            preflight(planned, Duration::from_secs(300))?;
            checked.push(planned.clone());
        }
    }
    let changes = if args.no_preflight {
        use_clients(paths, &selected, &request, false)?
    } else {
        use_clients_preflighted(paths, &selected, &request, &planned_specs)?
    };
    for change in changes {
        println!(
            "[{}:{}] {:?}",
            change.client,
            scope_name(change.scope),
            change.action
        );
    }
    Ok(())
}

fn revert_command(paths: &Paths, clients: &[Client], args: &RevertArgs) -> Result<(), FsError> {
    let selected = select_clients(clients, &args.selection.cli)
        .map_err(|error| FsError::new(error.to_string()))?;
    let request = RevertRequest { scope: args.scope };
    for change in revert_clients(paths, &selected, request, args.dry_run)? {
        let qualifier = if args.dry_run {
            "would restore"
        } else {
            "restored"
        };
        println!(
            "[{}:{}] {qualifier}",
            change.client,
            scope_name(change.scope)
        );
    }
    Ok(())
}

fn doctor(paths: &Paths, clients: &[Client], args: DoctorArgs) -> Result<(), FsError> {
    let repo = absolute_existing(&args.repo, "repository")?;
    let metadata = resolve_repo_meta(&repo, "tmux-mcp")?;
    let server = args.server.unwrap_or(metadata.server);
    println!("mcp-swap doctor");
    println!("  repo:   {}", repo.display());
    println!("  server: {server}");
    println!("  configurations:");
    for client in clients.iter().filter(|client| client.config_path.exists()) {
        match resolve_config_route(&client.config_path, 16 * 1024 * 1024) {
            Ok(route) => println!(
                "    {}: {} bytes, mode {:04o}",
                client.name.as_str(),
                route.file.identity.size,
                route.file.identity.mode
            ),
            Err(error) => println!("    {}: unreadable: {error}", client.name.as_str()),
        }
    }
    if paths.state_file().exists() {
        let ledger = load_ledger(&paths.state_file())?;
        println!("  outstanding swaps:");
        for entry in ledger.entries.values() {
            println!(
                "    {}:{} seq={} backup={}",
                entry.client,
                scope_name(entry.scope),
                entry.sequence,
                entry.backup_path.display()
            );
        }
    }
    for (name, client) in [
        ("ANTHROPIC_API_KEY", "claude"),
        ("OPENAI_API_KEY", "codex"),
        ("GEMINI_API_KEY", "gemini"),
        ("GOOGLE_API_KEY", "gemini"),
        ("XAI_API_KEY", "grok"),
        ("GROK_API_KEY", "grok"),
    ] {
        if std::env::var_os(name).is_some() {
            println!("  ! {name} overrides {client}'s stored login");
        }
    }
    Ok(())
}

fn selected_for_use<'a>(
    clients: &'a [Client],
    selectors: &[String],
) -> Result<Vec<&'a Client>, FsError> {
    if selectors.is_empty() {
        let detected: Vec<_> = clients
            .iter()
            .filter(|client| binary_on_path(client.binary) && client.config_path.exists())
            .collect();
        if detected.is_empty() {
            return Err(FsError::new(
                "no installed client with a configuration was detected",
            ));
        }
        return Ok(detected);
    }
    select_clients(clients, selectors).map_err(|error| FsError::new(error.to_string()))
}

fn selected_for_read<'a>(
    clients: &'a [Client],
    selectors: &[String],
) -> Result<Vec<&'a Client>, FsError> {
    if selectors.is_empty() {
        return Ok(clients
            .iter()
            .filter(|client| client.config_path.exists())
            .collect());
    }
    select_clients(clients, selectors).map_err(|error| FsError::new(error.to_string()))
}

fn binary_on_path(binary: &str) -> bool {
    if binary.contains('/') {
        return executable(Path::new(binary));
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|directory| executable(&directory.join(binary)))
    })
}

fn executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn absolute_existing(path: &Path, label: &str) -> Result<PathBuf, FsError> {
    path.canonicalize()
        .map_err(|error| FsError::new(format!("resolve {label} {}: {error}", path.display())))
}

fn parse_environment(raw: &str) -> Result<(String, String), String> {
    let (key, value) = raw
        .split_once('=')
        .ok_or_else(|| format!("--env expects KEY=VALUE, got {raw:?}"))?;
    if key.is_empty() {
        return Err(format!("--env expects KEY=VALUE, got {raw:?}"));
    }
    if key == "LIBTMUX_SAFETY" {
        return Err("LIBTMUX_SAFETY is retired; use LIBTMUX_TOOLSETS".into());
    }
    Ok((key.into(), value.into()))
}

const fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::User => "user",
        Scope::Project => "project",
    }
}
