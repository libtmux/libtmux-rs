use std::{
    fmt::Write as _,
    io::{IsTerminal as _, Write as _},
    path::PathBuf,
};

use clap::ArgMatches;
use libtmux::{
    Command, NewSessionOptions, NewWindowOptions, Server, Session, SplitDirection, SplitOptions,
    escape_format,
};
use serde_json::{Value, json};

use super::{
    CliError, Result, bridge, discovery, document, normalize,
    output::{Mode, Reporter},
    process,
};

/// The stage an input reaches when the session it names is already running
/// and is taken as it stands.
const REUSED: &str = "reused";

/// What one load settled before its first input, and every input then sees
/// unchanged: where it is aimed, what it may borrow, and how it ends.
struct LoadContext {
    server: Server,
    /// The checked Python runtime, present only when an input needs one.
    python: Option<std::ffi::OsString>,
    dimensions: Option<(u32, u32)>,
    borrowed: Option<AppendTarget>,
    /// The answer to the one question an interactive load asks before it
    /// builds anything: a terminal, no `--yes`, and a load that would attach.
    /// The question names the last input, the session the load would end
    /// on, and the answer governs every input the way the matching flag
    /// would; only a declined attach is about that one input alone.
    disposition: Option<Disposition>,
    /// Whether the load is inside tmux on the server it targets. `None` when
    /// `-d` or `--append` mean it never attaches.
    inside_tmux: Option<bool>,
    /// Whether the load attaches at the end before any prompt says otherwise.
    attach: bool,
}

#[derive(Default)]
struct Effects {
    input: usize,
    path: Option<String>,
    session: Option<Session>,
    owned: bool,
    changed: bool,
    mutation_started: Option<&'static str>,
    windows: Vec<String>,
    panes: Vec<String>,
    stage: &'static str,
    script_output: Option<Value>,
}

#[derive(Default)]
pub(super) struct LoadState {
    started: bool,
    current: Option<Effects>,
    results: Vec<Value>,
    completed: Option<Value>,
}

impl LoadState {
    fn failure(&self, error: &CliError) -> Value {
        let mut failure = json!({"code":error.code,"message":error.message});
        let mut partial = !self.results.is_empty();
        let mut results = self.results.clone();
        if let Some(effects) = &self.current {
            let uncertain = error.code == "interrupted" && effects.mutation_started.is_some();
            partial |= effects.changed || uncertain;
            failure["input_index"] = json!(effects.input);
            failure["partial_effects"] = json!(effects.changed);
            failure["effects"] = effects.value();
            if uncertain {
                failure["outcome_unknown"] = json!(true);
                failure["mutation_stage"] = json!(effects.mutation_started);
            }
            // One record per input attempted, the failed one included, with
            // the same minimum fields a completed input's record carries --
            // unless it is already there (this input built, and only the
            // summary write after it failed) or the outcome is genuinely
            // unknown (interrupted), where inventing one would contradict
            // outcome_unknown's own guarantee.
            let already_recorded = self
                .results
                .iter()
                .any(|record| record["input_index"] == effects.input);
            if !uncertain && !already_recorded {
                results.push(json!({
                    "input": effects.path,
                    "input_index": effects.input,
                    "session_id": effects.session.as_ref().map(|s| s.id().to_string()),
                    "session_name": effects.session.as_ref().map(|s| s.name().to_string_lossy()),
                    "reused": effects.stage == REUSED,
                }));
            }
        }
        json!({"schema_version":1,"command":"load","status":if partial {"partial"} else {"error"},"errors":[failure],"results":results})
    }

    pub(super) fn interrupted(&self, error: &mut CliError, report: &mut Reporter) {
        if !self.started {
            return;
        }
        let summary = self
            .completed
            .clone()
            .unwrap_or_else(|| self.failure(error));
        if self.completed.is_none() {
            if let Err(publication) = report.summary("failed", &summary) {
                let _ = write!(error.message, "; output failed: {publication}");
            }
        }
        error.retained_state = Some(summary);
    }
}

impl Effects {
    fn value(&self) -> Value {
        json!({"input_index":self.input,"session_id":self.session.as_ref().map(|s|s.id().to_string()),"session_name":self.session.as_ref().map(|s|s.name().to_string_lossy()),"owned_session":self.owned,"window_ids":self.windows,"pane_ids":self.panes,"stage":self.stage,"script_output":self.script_output})
    }
}

fn option<'a>(args: &'a ArgMatches, name: &str) -> Option<&'a String> {
    args.try_get_one::<String>(name).ok().flatten()
}

fn flag(args: &ArgMatches, name: &str) -> bool {
    args.try_get_one::<bool>(name)
        .ok()
        .flatten()
        .copied()
        .unwrap_or(false)
}

pub(super) fn server(args: &ArgMatches) -> Result<Server> {
    let mut builder = Server::builder();
    if let Some(path) = option(args, "socket-path") {
        builder = builder.socket_path(discovery::expand(path));
    } else if let Some(name) = option(args, "socket-name") {
        builder = builder.socket_name(name);
    } else if let Ok(context) = std::env::var("TMUX") {
        if !context.is_empty() {
            let (socket, _) = tmux_context(&context)?;
            builder = builder.socket_path(socket);
        }
    }
    if let Some(config) = option(args, "tmux-config") {
        builder = builder.config_file(config);
    }
    if flag(args, "colors256") {
        builder = builder.colors(256);
    }
    if let Some(executable) = std::env::var_os("LIBTMUX_TEST_TMUX") {
        builder = builder.tmux_executable(executable);
    }
    Ok(builder.build()?)
}

pub(super) async fn selected_session(server: &Server, name: Option<&str>) -> Result<Session> {
    if let Some(name) = name {
        // A socket with no server behind it holds no session either, so it is
        // the answer a name that is not running gets, not a transport error.
        let found = match server.session(name).await {
            Err(libtmux::Error::ServerGone { .. }) => None,
            other => other?,
        };
        return found.ok_or_else(|| {
            CliError::new(
                "session_not_found",
                format!("session {name:?} was not found"),
            )
        });
    }
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        let result = server
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("-t")
                    .arg(pane)
                    .arg("#{session_name}"),
            )
            .await?;
        let name = result.stdout_lossy().trim().to_owned();
        if let Some(session) = server.session(name).await? {
            return Ok(session);
        }
    }
    let mut sessions = match server.sessions().await {
        Err(libtmux::Error::ServerGone { .. }) => Vec::new(),
        other => other?,
    };
    if sessions.is_empty() {
        return Err(CliError::new(
            "session_not_found",
            "no sessions are running",
        ));
    }
    if sessions.len() == 1 {
        return sessions
            .pop()
            .ok_or_else(|| CliError::new("session_not_found", "no session"));
    }
    Err(CliError::new(
        "session_not_found",
        "more than one session is running; select one by name",
    ))
}

fn load_inputs(args: &ArgMatches) -> Result<Vec<(PathBuf, normalize::Workspace)>> {
    let files = args
        .get_many::<String>("workspace_files")
        .ok_or_else(|| CliError::usage("workspace files are required"))?;
    let mut workspaces = Vec::new();
    let count = files.len();
    for (index, file) in files.enumerate() {
        let path = discovery::resolve(file, None)?;
        let mut workspace = normalize::workspace(&document::read(&path)?, &path)?;
        if index + 1 == count {
            if let Some(name) = option(args, "session-name") {
                workspace.name.clone_from(name);
            }
        }
        workspaces.push((path, workspace));
    }
    if workspaces
        .iter()
        .any(|(_, workspace)| workspace.before_script.is_some() || workspace.bridge)
    {
        process::require_support()?;
    }
    Ok(workspaces)
}

fn native_layouts(
    workspaces: &[(PathBuf, normalize::Workspace)],
) -> impl Iterator<Item = (&std::ffi::OsStr, usize)> {
    workspaces
        .iter()
        .filter(|(_, workspace)| !workspace.bridge)
        .flat_map(|(_, workspace)| workspace.windows.iter())
        .filter_map(|window| {
            window
                .layout
                .as_deref()
                .filter(|layout| !layout.is_empty())
                .map(|layout| (std::ffi::OsStr::new(layout), window.panes.len().max(1)))
        })
}

pub(super) async fn load(
    args: &ArgMatches,
    report: &mut Reporter,
    state: &mut LoadState,
) -> Result<()> {
    state.started = true;
    if flag(args, "colors88") {
        return Err(CliError::usage(
            "tmux 3.2a and newer do not support 88-color mode; omit -8 or use -2",
        ));
    }
    if report.machine() && !flag(args, "detached") && !flag(args, "append") {
        return Err(CliError::usage("machine load requires -d or --append"));
    }
    let (workspaces, context) = load_setup(args, report).await?;
    let mut last_session = None;
    let mut attach_at_end = context.attach;
    let mut appended = Vec::new();
    report.event("started", json!({"inputs":workspaces.len()}))?;
    for (index, (path, workspace)) in workspaces.iter().enumerate() {
        let effects = state.current.insert(Effects {
            input: index,
            path: Some(discovery::masked(path)),
            ..Effects::default()
        });
        let outcome = load_one(
            &context,
            workspace,
            index + 1 == workspaces.len(),
            args,
            report,
            effects,
            &mut state.results,
        )
        .await;
        match outcome {
            Ok((session, attach_this_input, appended_flag)) => {
                appended.push(appended_flag);
                last_session = Some(session);
                attach_at_end = attach_this_input;
            }
            Err(mut error) => {
                let summary = state.failure(&error);
                if let Err(publication) = report.summary("failed", &summary) {
                    let _ = write!(error.message, "; output failed: {publication}");
                }
                error.retained_state = Some(summary);
                return Err(error);
            }
        }
    }
    let mut summary = json!({"schema_version":1,"command":"load","status":"ok","errors":[]});
    summary["results"] = state.results.clone().into();
    state.completed = Some(summary.clone());
    let outcome = async {
        report.summary("completed", &summary)?;
        if !report.machine() {
            report.loaded(&summary["results"], &appended)?;
            std::io::Write::flush(&mut std::io::stdout())?;
            report.log_warning();
            if attach_at_end {
                if let Some(session) = last_session {
                    attach(
                        &context.server,
                        &session,
                        context.inside_tmux.unwrap_or(false),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }
    .await;
    outcome.map_err(|mut error: CliError| {
        error.retained_state = Some(summary);
        error
    })
}

/// What a load's interactive prompt decided, beyond the load's own flags.
enum Disposition {
    /// Build (or reuse) and attach at the end, same as the unprompted default.
    Switch,
    /// Build, but leave the client where it is.
    Detached,
    /// Append into the answering prompt's own `AppendTarget`.
    Append(AppendTarget),
    /// The session already exists and the answer was no: change nothing.
    Decline,
}

/// Asks whether to switch, load detached or append (a session that does not
/// yet exist, inside tmux), or attach (one that already does), about a load's
/// last input. Only reached when stdin is a terminal, `--yes` was not given,
/// and the load is neither detached nor appending.
async fn prompt_disposition(
    server: &Server,
    workspace: &normalize::Workspace,
    inside_tmux: bool,
) -> Result<Disposition> {
    let existing = if server.is_alive().await {
        server.session(&workspace.name).await?
    } else {
        None
    };
    if existing.is_some() {
        let answer = ask(
            &format!("{} is already running. Attach? [Y/n]", workspace.name),
            "yn",
            'y',
        )?;
        return Ok(if answer == 'n' {
            Disposition::Decline
        } else {
            Disposition::Switch
        });
    }
    if !inside_tmux {
        return Ok(Disposition::Switch);
    }
    let answer = ask(
        "Already inside tmux: switch (y), load detached (n), or append (a)? [y/n/a]",
        "yna",
        'y',
    )?;
    Ok(match answer {
        'n' => Disposition::Detached,
        'a' => Disposition::Append(append_target(server).await?),
        _ => Disposition::Switch,
    })
}

/// Reads one keystroke answer from a real terminal, defaulting on anything
/// else so a garbled line never hangs the prompt.
fn ask(question: &str, choices: &str, default: char) -> Result<char> {
    write!(std::io::stderr(), "{question} ")?;
    std::io::stderr().flush()?;
    let mut response = String::new();
    std::io::stdin().read_line(&mut response)?;
    Ok(response
        .trim()
        .chars()
        .next()
        .map(|answer| answer.to_ascii_lowercase())
        .filter(|answer| choices.contains(*answer))
        .unwrap_or(default))
}

async fn load_setup(
    args: &ArgMatches,
    report: &mut Reporter,
) -> Result<(Vec<(PathBuf, normalize::Workspace)>, LoadContext)> {
    let workspaces = load_inputs(args)?;
    let server = server(args)?;
    // Whether this load is inside tmux on the server it targets, and
    // whether it can attach at all: `None` when `-d`/`--append` mean it
    // never will. Resolved, and any cross-server refusal raised, before any
    // tmux call this load makes.
    let inside_tmux = if !flag(args, "detached") && !flag(args, "append") {
        Some(require_attach_context(&server).await?)
    } else {
        None
    };
    // A malformed COLUMNS/LINES/TMUXP_DEFAULT_* is a usage mistake; fail
    // before any target lookup or mutation, not mid-load as a build error.
    let dimensions = session_dimensions()?;
    report.progress = super::progress::Progress::new(args, report.machine())?;
    if let Some(path) = option(args, "log-file") {
        report
            .log
            .open(std::path::Path::new(&discovery::expand(path)))?;
    }
    // `-d` always wins over `--append`: a detached load never borrows the
    // current pane's session, inside tmux or outside it.
    let borrowed = if flag(args, "append") && !flag(args, "detached") {
        Some(append_target(&server).await?)
    } else {
        None
    };
    server.validate_layouts(native_layouts(&workspaces)).await?;
    warn_about_inputs(&workspaces, report)?;
    let python = if workspaces.iter().any(|(_, workspace)| workspace.bridge) {
        Some(process::python().await?)
    } else {
        None
    };
    let attach = !flag(args, "detached") && !flag(args, "append");
    let interactive =
        !report.machine() && !flag(args, "yes") && attach && std::io::stdin().is_terminal();
    let disposition = match workspaces.last() {
        Some((_, last)) if interactive => {
            Some(prompt_disposition(&server, last, inside_tmux.unwrap_or(false)).await?)
        }
        _ => None,
    };
    Ok((
        workspaces,
        LoadContext {
            server,
            python,
            dimensions,
            borrowed,
            disposition,
            inside_tmux,
            attach,
        },
    ))
}

/// What a load carries on past but a person should know: a setting written
/// for another builder, and a directory that is not there, which tmux
/// silently replaces with `$HOME`. Said once each, before anything is built.
fn warn_about_inputs(
    workspaces: &[(PathBuf, normalize::Workspace)],
    report: &mut Reporter,
) -> Result<()> {
    let mut said = std::collections::BTreeSet::new();
    for (_, workspace) in workspaces {
        for warning in &workspace.warnings {
            report.warn("unsupported_builder_option", warning)?;
        }
        let panes = workspace
            .windows
            .iter()
            .flat_map(|window| window.panes.iter().map(|pane| &pane.directory));
        for directory in std::iter::once(&workspace.directory).chain(panes) {
            if directory.is_dir() || !said.insert(directory.clone()) {
                continue;
            }
            report.warn(
                "start_directory_absent",
                &format!(
                    "start_directory is not a directory, so tmux will start the pane in $HOME: {}",
                    discovery::masked(directory)
                ),
            )?;
        }
    }
    Ok(())
}

/// Builds (or reuses, or appends) one workspace input, as the load's flags
/// and any answered prompt direct. Returns the session, whether the load should
/// attach to it at the end, and whether this input was appended rather than
/// created or reused.
///
/// The result record is pushed to `results` before the `workspace-completed`
/// event that carries it is sent: a consumer that closes its read end right
/// after seeing that event must still find the record it just saw.
async fn load_one(
    context: &LoadContext,
    workspace: &normalize::Workspace,
    asked_about: bool,
    args: &ArgMatches,
    report: &mut Reporter,
    effects: &mut Effects,
    results: &mut Vec<Value>,
) -> Result<(Session, bool, bool)> {
    let inherited = context.borrowed.as_ref();
    let disposition = context.disposition.as_ref();
    let declined = asked_about && matches!(disposition, Some(Disposition::Decline));
    let (borrowed_for_input, attach_this_input) = match disposition {
        None => (inherited, context.attach),
        Some(Disposition::Switch) => (inherited, true),
        Some(Disposition::Detached | Disposition::Decline) => (inherited, false),
        Some(Disposition::Append(target)) => (Some(target), false),
    };
    if let Some(progress) = &mut report.progress {
        progress.start(workspace)?;
    }
    let input = json!({"input_index":effects.input,"input":effects.path});
    report.event("workspace-started", input.clone())?;
    let built = build(
        context,
        workspace,
        args,
        report,
        effects,
        borrowed_for_input,
    )
    .await;
    let (session, reused) = match built {
        Ok(built) => built,
        Err(error) => return Err(rolled_back(error, effects).await),
    };
    // Reusing a session is a claim that the workspace is already there, so it
    // is checked rather than assumed; converging one that is not is a
    // separate job this does not do. Declining the prompt asked for no
    // workspace at all, and the extension route builds through tmuxp, which
    // decides for itself what reuse means. A session found but found wanting
    // is `session_mismatch`, not `session_not_found`: it is right there, so a
    // consumer branching on "go find it" would look forever. Nothing was
    // built or changed, so this reports `error`, never `partial`.
    if reused && !workspace.bridge && !declined {
        let missing = missing_windows(&session, workspace).await?;
        if let Some(first) = missing.first() {
            return Err(CliError::new(
                "session_mismatch",
                format!(
                    "session {:?} is already running without {first}; nothing was changed",
                    workspace.name
                ),
            ));
        }
    }
    let mut result = effects.value();
    result["input"] = input["input"].clone();
    result["reused"] = json!(reused);
    // A declined attach compared nothing and changed nothing: `reused` is
    // still true because the session was found, but the report must not
    // call that a reuse, which is what "declined" says instead.
    result["declined"] = json!(declined);
    let appended = borrowed_for_input.is_some();
    results.push(result.clone());
    if let Some(progress) = &mut report.progress {
        progress.finish(reused, workspace.bridge)?;
    }
    report.event("workspace-completed", result)?;
    Ok((session, attach_this_input, appended))
}

struct AppendTarget {
    session: Session,
    identity: (u32, u64),
}

impl AppendTarget {
    async fn recheck(&self, server: &Server) -> Result<()> {
        let observed = target_context(server, self.session.id().as_ref()).await?;
        if (observed.0, observed.1) != self.identity || &observed.2 != self.session.id() {
            return Err(append_context(
                "borrowed daemon or session identity changed",
            ));
        }
        Ok(())
    }
}

/// An append whose inherited context no longer holds is a refusal about how
/// the command was invoked, the same family as an unusable pane.
fn append_context(message: impl Into<String>) -> CliError {
    CliError::usage(message)
}

/// The one reader of `TMUX` in this command.
///
/// Splitting from the right keeps a socket path that contains commas, which
/// tmux itself truncates at the first one; every path that resolves an
/// endpoint from the variable goes through here so that the same value never
/// names two different servers.
///
/// This is a deliberate, verified difference from tmux, not an oversight:
/// tmux's own `main()` (`tmux.c`) resolves `$TMUX` with
/// `path[strcspn(path, ",")] = '\0'`, the first comma, so a socket path
/// holding one truncates there for tmux itself. Splitting from the right
/// instead means this command can resolve a `$TMUX` that tmux's own client
/// would treat as naming a different, truncated socket. The trade accepted:
/// every reader inside this command agreeing with itself, over agreeing with
/// a tmux binary that would refuse the same variable outright — a client run
/// against a comma-bearing socket path already gets a truncated `$TMUX`
/// tmux cannot use to reattach either way.
fn tmux_context(context: &str) -> Result<(&str, u32)> {
    let parsed = context.rsplit_once(',').and_then(|(prefix, session)| {
        session.parse::<u32>().ok()?;
        let (socket, pid) = prefix.rsplit_once(',')?;
        let pid = pid.parse::<u32>().ok()?;
        (!socket.is_empty() && pid > 0).then_some((socket, pid))
    });
    parsed.ok_or_else(|| {
        CliError::usage("TMUX is set to something other than socket,pid,session, so the current tmux server cannot be identified")
    })
}

async fn target_context(server: &Server, target: &str) -> Result<(u32, u64, libtmux::SessionId)> {
    let result = server
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("-t")
                .arg(target)
                .arg("#{pid}:#{start_time}:#{session_id}"),
        )
        .await
        .map_err(|_| append_context("the tmux server holding the current pane is not running"))?;
    let text = result.stdout_lossy();
    let mut fields = text.trim().split(':');
    let parsed = (|| {
        let pid = fields.next()?.parse::<u32>().ok()?;
        let started = fields.next()?.parse::<u64>().ok()?;
        let session = fields.next()?.parse::<libtmux::SessionId>().ok()?;
        (pid > 0 && started > 0 && fields.next().is_none()).then_some((pid, started, session))
    })();
    parsed.ok_or_else(|| append_context("target has no live daemon and session identity"))
}

async fn append_target(server: &Server) -> Result<AppendTarget> {
    let pane = std::env::var("TMUX_PANE")
        .map_err(|_| CliError::usage("append requires TMUX_PANE identifying the current pane"))?
        .parse::<libtmux::PaneId>()
        .map_err(|error| append_context(error.to_string()))?;
    let context = std::env::var("TMUX")
        .map_err(|_| CliError::usage("append requires the inherited TMUX daemon identity"))?;
    let (socket, pid) = tmux_context(&context)?;
    let inherited_server = Server::builder()
        .socket_path(socket)
        .tmux_executable(server.tmux_executable())
        .build()?;
    let inherited = target_context(&inherited_server, pane.as_ref()).await?;
    let selected = target_context(server, pane.as_ref()).await?;
    if inherited.0 != pid || inherited != selected {
        return Err(append_context(
            "inherited TMUX and selected endpoint do not identify the same live daemon and session",
        ));
    }
    let session = server
        .session_by_id(&selected.2)
        .await?
        .ok_or_else(|| append_context("current pane session no longer exists"))?;
    Ok(AppendTarget {
        session,
        identity: (selected.0, selected.1),
    })
}

async fn build(
    context: &LoadContext,
    workspace: &normalize::Workspace,
    args: &ArgMatches,
    report: &mut Reporter,
    effects: &mut Effects,
    borrowed: Option<&AppendTarget>,
) -> Result<(Session, bool)> {
    let server = &context.server;
    if let Some(target) = borrowed {
        effects.session = Some(target.session.clone());
        effects.stage = "append-validation";
        target.recheck(server).await?;
    }
    if workspace.bridge {
        return build_extension(context, workspace, args, report, effects, borrowed).await;
    }
    let input = effects.input;
    let append = borrowed.is_some();
    if !append && server.is_alive().await {
        if let Some(session) = server.session(&workspace.name).await? {
            effects.session = Some(session.clone());
            effects.stage = REUSED;
            return Ok((session, true));
        }
    }
    let session = if let Some(target) = borrowed {
        target.session.clone()
    } else {
        effects.stage = "session-creation";
        effects.mutation_started = Some(effects.stage);
        let mut options = NewSessionOptions::new(escape_format(&workspace.name))
            .start_directory(escape_format(&workspace.directory));
        // No `-x`/`-y` at all with detection disabled: tmux then sizes the
        // session from the largest attached client, or `default-size`.
        if let Some((columns, rows)) = context.dimensions {
            options = options.size(columns, rows);
        }
        let session = server.new_session(options).await?;
        effects.session = Some(session.clone());
        effects.owned = true;
        effects.changed = true;
        effects.stage = "session-created";
        report.event("session-created", json!({"input_index":input,"session_id":session.id().to_string(),"session_name":workspace.name}))?;
        session
    };
    effects.session = Some(session.clone());
    // Every pane waits for its shell to be ready before the first command is
    // typed, whatever shell that is: text sent to a terminal the line editor
    // does not own yet is echoed and then redrawn, so the command reads twice.
    let readiness = workspace.readiness.unwrap_or(true);
    configure_session(server, &session, workspace, report, effects, borrowed).await?;
    let mut bootstrap = if append {
        None
    } else {
        session.active_window().await?
    };
    if let Some(window) = &mut bootstrap {
        let spare = workspace
            .windows
            .iter()
            .filter_map(|w| w.index)
            .max()
            .unwrap_or(0)
            .checked_add(1000)
            .ok_or_else(|| CliError::invalid("window index leaves no temporary bootstrap slot"))?;
        window.move_to(&session, spare).await?;
    }
    let mut selected = None;
    for (window_index, config) in workspace.windows.iter().enumerate() {
        let window =
            build_window(&session, config, report, effects, window_index, readiness).await?;
        // Appending is a guest in a session the client already owns: only an
        // explicit `focus: true` earns a switch. A fresh build still falls
        // back to its first window, the way it always has.
        if config.focus || (!append && selected.is_none()) {
            selected = Some(window);
        }
    }
    if let Some(window) = bootstrap {
        window.kill().await?;
    }
    if let Some(mut window) = selected {
        window.select().await?;
    }
    effects.stage = "completed";
    Ok((session, false))
}

/// Which of the document's windows the session does not hold, named the way
/// the document names them. Matched as a multiset so duplicate and unnamed
/// window entries each consume one window rather than all matching the same
/// one.
async fn missing_windows(
    session: &Session,
    workspace: &normalize::Workspace,
) -> Result<Vec<String>> {
    let mut present = session
        .windows()
        .await?
        .iter()
        .map(|window| window.name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut missing = Vec::new();
    for (index, window) in workspace.windows.iter().enumerate() {
        let found = match &window.name {
            Some(name) => present.iter().position(|held| held == name),
            None => (!present.is_empty()).then_some(0),
        };
        if let Some(found) = found {
            present.remove(found);
        } else {
            missing.push(window.name.clone().map_or_else(
                || format!("a {} window", ordinal(index + 1)),
                |name| format!("window {name:?}"),
            ));
        }
    }
    Ok(missing)
}

fn ordinal(position: usize) -> String {
    let suffix = match (position % 10, position % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{position}{suffix}")
}

/// A borrowed session is the caller's; only one this load created is ours
/// to remove. Once it is, nothing this input did is left behind, so a
/// caller checking whether effects persisted sees none.
async fn remove_owned_session(effects: &mut Effects) -> Result<bool> {
    if !effects.owned {
        return Ok(false);
    }
    let Some(session) = effects.session.clone() else {
        return Ok(false);
    };
    session.kill().await?;
    effects.changed = false;
    effects.windows.clear();
    effects.panes.clear();
    Ok(true)
}

/// A load that reports failure leaves nothing it created behind, the
/// bootstrap window with it; a session it borrowed is the caller's and keeps
/// whatever was added, named so the caller can find it. An interrupted
/// mutation is neither: what it touched is genuinely unknown, so it is
/// reported rather than undone.
async fn rolled_back(mut error: CliError, effects: &mut Effects) -> CliError {
    if error.code == "interrupted" {
        return error;
    }
    if !effects.owned {
        if !effects.windows.is_empty() {
            let _ = write!(
                error.message,
                "; the windows it added were kept: {}",
                effects.windows.join(", ")
            );
        }
        return error;
    }
    match remove_owned_session(effects).await {
        Ok(true) => error.message.push_str("; the session was removed"),
        Ok(false) => {}
        Err(removal) => {
            let _ = write!(
                error.message,
                "; the session could not be removed: {}",
                removal.message
            );
        }
    }
    error
}

async fn configure_session(
    server: &Server,
    session: &Session,
    workspace: &normalize::Workspace,
    report: &mut Reporter,
    effects: &mut Effects,
    borrowed: Option<&AppendTarget>,
) -> Result<()> {
    if let Some(script) = &workspace.before_script {
        effects.stage = "before-script";
        effects.changed = true;
        effects.mutation_started = Some(effects.stage);
        let mut argv = process::split(script)?;
        if let Some(executable) = argv
            .first_mut()
            .filter(|value| value.as_encoded_bytes().starts_with(b"."))
        {
            let parent = workspace
                .source
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."));
            *executable = parent.join(&*executable).into_os_string();
        }
        let output = match process::run(
            &argv,
            &workspace.script_directory,
            report,
            Some(effects.input),
            &[],
        )
        .await
        {
            Ok(output) => output,
            // Missing or not executable: tmuxp's BeforeLoadScriptNotExists,
            // the same failure as a nonzero exit, not a different one.
            Err(error) if error.code == "child_spawn" => {
                return Err(CliError {
                    code: "script_failed",
                    message: format!("before_script {}", error.message),
                    status: 1,
                    retained_state: None,
                });
            }
            Err(error) => return Err(error),
        };
        effects.script_output = Some(output.value());
        if matches!(output.status, 130 | 143) {
            // 130/143 are the child's own SIGINT/SIGTERM death (128 +
            // signal), the same signals `interruptible` listens for: the
            // whole terminal's foreground group, this process included, can
            // deliver one straight to the child before the async handler
            // here is even polled. Report it exactly as that handler would:
            // changes are not rolled back, matching every other interrupted
            // mutation, not the script-failed path below.
            return Err(CliError {
                code: "interrupted",
                message: "operation interrupted".into(),
                status: 130,
                retained_state: None,
            });
        }
        if output.status != 0 {
            // Its own exit status is not ours: tmuxp and every other port
            // exit 1 here, not the script's code.
            return Err(CliError {
                code: "script_failed",
                message: format!("before_script exited with status {}", output.status),
                status: 1,
                retained_state: None,
            });
        }
        if let Some(target) = borrowed {
            target.recheck(server).await?;
        }
    }
    effects.stage = "session-options";
    effects.changed |= !workspace.environment.is_empty()
        || !workspace.options.is_empty()
        || !workspace.global_options.is_empty();
    if effects.changed {
        effects.mutation_started = Some(effects.stage);
    }
    for (name, value) in &workspace.environment {
        session.set_environment(name, value).await?;
    }
    for (name, value) in &workspace.options {
        session.set_option(name, value).await?;
    }
    for (name, value) in &workspace.global_options {
        // tmuxp applies this as `set-option -g` (session table); the
        // server table (`-s`) refuses most option names outright.
        server.set_global_option(name, value).await?;
    }
    Ok(())
}

async fn build_extension(
    context: &LoadContext,
    workspace: &normalize::Workspace,
    args: &ArgMatches,
    report: &mut Reporter,
    effects: &mut Effects,
    borrowed: Option<&AppendTarget>,
) -> Result<(Session, bool)> {
    let server = &context.server;
    let python = context
        .python
        .as_deref()
        .ok_or_else(|| CliError::new("script_failed", "checked Python runtime is missing"))?;
    let append = borrowed.is_some();
    let prior = if let Some(target) = borrowed {
        Some(target.session.clone())
    } else if server.is_alive().await {
        server.session(&workspace.name).await?
    } else {
        None
    };
    let mut windows = std::collections::BTreeSet::new();
    let mut panes = std::collections::BTreeSet::new();
    if let Some(session) = &prior {
        for window in session.windows().await? {
            windows.insert(window.id().to_string());
            for pane in window.panes().await? {
                panes.insert(pane.id().to_string());
            }
        }
        effects.session = Some(session.clone());
    }
    let request = json!({"path":workspace.source,"session_name":workspace.name,"socket":server.socket_path(),"append":borrowed.map(|target|target.session.id().to_string()),"append_identity":borrowed.map(|target|format!("{}:{}",target.identity.0,target.identity.1)),"config_file":option(args,"tmux-config"),"colors":if flag(args,"colors256"){Some(256)}else{None}});
    effects.stage = "python-extension";
    effects.mutation_started = Some(effects.stage);
    let output = bridge::build(python, request, report).await?;
    effects.script_output = Some(output.value());
    let current = if append {
        prior.clone()
    } else if server.is_alive().await {
        server.session(&workspace.name).await?
    } else {
        None
    };
    effects.owned = prior.is_none() && current.is_some();
    if let Some(session) = &current {
        effects.session = Some(session.clone());
        for window in session.windows().await? {
            if !windows.contains(&window.id().to_string()) {
                effects.windows.push(window.id().to_string());
            }
            for pane in window.panes().await? {
                if !panes.contains(&pane.id().to_string()) {
                    effects.panes.push(pane.id().to_string());
                }
            }
        }
    }
    effects.changed = effects.owned || !effects.windows.is_empty() || append;
    output
        .success()
        .map_err(|error| CliError::new("script_failed", error.message))?;
    effects.stage = "completed";
    let session = current.ok_or_else(|| {
        CliError::new(
            "script_failed",
            "the workspace builder finished without a session",
        )
    })?;
    Ok((session, !append && prior.is_some()))
}

async fn build_window(
    session: &Session,
    config: &normalize::Window,
    report: &mut Reporter,
    effects: &mut Effects,
    window_index: usize,
    readiness: bool,
) -> Result<libtmux::Window> {
    if let Some(progress) = &mut report.progress {
        progress.window(window_index + 1, config)?;
    }
    let input = effects.input;
    effects.stage = "window-creation";
    effects.mutation_started = Some(effects.stage);
    let first = &config.panes[0];
    let mut options = config
        .name
        .as_ref()
        .map_or_else(NewWindowOptions::unnamed, |name| {
            NewWindowOptions::new(escape_format(name))
        });
    options = options.start_directory(escape_format(&first.directory));
    if let Some(index) = config.index {
        options = options.index(index);
    }
    if let Some(shell) = &first.shell {
        options = options.command(shell);
    }
    for (name, value) in &first.environment {
        options = options.environment(name, value);
    }
    let mut window = session.new_window(options).await?;
    effects.changed = true;
    effects.windows.push(window.id().to_string());
    report.event("window-created", json!({"input_index":input,"session_id":session.id().to_string(),"window_index":window.index(),"window_id":window.id().to_string()}))?;
    for (name, value) in &config.options {
        window.set_option(name, value).await?;
    }
    let mut panes = vec![
        window
            .active_pane()
            .await?
            .ok_or_else(|| CliError::new("tmux_failed", "the new window has no pane"))?,
    ];
    effects.panes.push(panes[0].id().to_string());
    // Split from the previous pane, not the window: `-t <window>` always
    // divides the active pane, which a detached split never changes.
    let mut source = panes[0].clone();
    for pane_config in config.panes.iter().skip(1) {
        let mut split = SplitOptions::new(SplitDirection::Below)
            .start_directory(escape_format(&pane_config.directory));
        if let Some(shell) = &pane_config.shell {
            split = split.command(shell);
        }
        for (name, value) in &pane_config.environment {
            split = split.environment(name, value);
        }
        let pane = source.split(split).await?;
        effects.panes.push(pane.id().to_string());
        source = pane.clone();
        panes.push(pane);
        window.select_layout(libtmux::Layout::Tiled).await?;
    }
    if let Some(layout) = config.layout.as_deref().filter(|layout| !layout.is_empty()) {
        window.select_layout(layout).await?;
    }
    // With nothing claiming focus the window is left on the pane it finished
    // making, which is where tmuxp leaves it and where a person reading the
    // window last looked. A detached split never moves the active pane, so
    // this has to be asked for.
    let mut active = panes.last().cloned();
    for (pane_index, (pane, config)) in panes.iter().zip(&config.panes).enumerate() {
        if let Some(progress) = &mut report.progress {
            progress.pane(pane_index + 1)?;
        }
        let pane_fields = json!({"input_index":input,"session_id":session.id().to_string(),"window_id":window.id().to_string(),"pane_id":pane.id().to_string(),"pane_index":pane_index});
        report.event("pane-created", pane_fields.clone())?;
        if readiness && config.shell.is_none() {
            wait_for_prompt(pane, report).await?;
        }
        send_commands(pane, &config.commands).await?;
        if let Some(progress) = &mut report.progress {
            progress.pane_done()?;
        }
        report.event("pane-completed", pane_fields)?;
        if config.focus {
            active = Some(pane.clone());
        }
    }
    if let Some(mut pane) = active {
        pane.select().await?;
    }
    effects.stage = "window-options-after";
    for (name, value) in &config.options_after {
        window.set_option(name, value).await?;
    }
    if let Some(progress) = &mut report.progress {
        progress.window_done()?;
    }
    report.event("window-completed", json!({"input_index":input,"session_id":session.id().to_string(),"window_index":window.index(),"window_id":window.id().to_string()}))?;
    Ok(window)
}

async fn send_commands(pane: &libtmux::Pane, commands: &[normalize::TypedCommand]) -> Result<()> {
    for command in commands {
        tokio::time::sleep(command.before).await;
        if command.enter {
            pane.send_line(&command.text).await?;
        } else {
            pane.send_keys(&command.text).await?;
        }
        tokio::time::sleep(command.after).await;
    }
    Ok(())
}

async fn wait_for_prompt(pane: &libtmux::Pane, report: &mut Reporter) -> Result<()> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        match pane.format("#{cursor_x},#{cursor_y}").await {
            Ok(cursor) if cursor.to_string_lossy() != "0,0" => return Ok(()),
            Ok(_) => {}
            // A refused probe is a different fact from an unmoved cursor, and
            // reporting it as the deadline would claim an observation that
            // never happened.
            Err(error) => {
                return report.event("warning", json!({"code":"pane_readiness_unreadable","pane_id":pane.id().to_string(),"message":format!("pane state could not be read: {error}; continuing as tmuxp does")}));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    report.event("warning", json!({"code":"pane_readiness_timeout","pane_id":pane.id().to_string(),"message":"shell prompt was not observed within two seconds; continuing as tmuxp does"}))
}

/// One of `TMUXP_DEFAULT_COLUMNS`/`COLUMNS` or `TMUXP_DEFAULT_ROWS`/`ROWS`,
/// first name found wins. Unset or empty falls through; a value present but
/// out of tmux's `1..=65535` window size is a usage error, not a default.
fn env_dimension(names: &[&str]) -> Result<Option<u32>> {
    for name in names {
        match std::env::var(name) {
            Ok(raw) if !raw.is_empty() => {
                return raw
                    .parse::<u32>()
                    .ok()
                    .filter(|value| (1..=65535).contains(value))
                    .map(Some)
                    .ok_or_else(|| CliError::usage(format!("{name} must be 1..65535")));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// Mirrors tmuxp's `TMUXP_DETECT_TERMINAL_SIZE`: `None` means pass no
/// `-x`/`-y` at all. Otherwise start from `TMUXP_DEFAULT_COLUMNS`/`ROWS` (else
/// `COLUMNS`/`ROWS`, else 80x24), let the invoking terminal override it when
/// stdout is one, then let `COLUMNS`/`LINES` override the terminal.
fn session_dimensions() -> Result<Option<(u32, u32)>> {
    let mut width = env_dimension(&["TMUXP_DEFAULT_COLUMNS", "COLUMNS"])?.unwrap_or(80);
    let mut height = env_dimension(&["TMUXP_DEFAULT_ROWS", "ROWS"])?.unwrap_or(24);
    if std::env::var("TMUXP_DETECT_TERMINAL_SIZE").is_ok_and(|value| value != "1") {
        return Ok(None);
    }
    if let Ok(winsize) = rustix::termios::tcgetwinsize(std::io::stdout()) {
        if winsize.ws_col > 0 && winsize.ws_row > 0 {
            width = u32::from(winsize.ws_col);
            height = u32::from(winsize.ws_row);
        }
    }
    if let Some(value) = env_dimension(&["COLUMNS"])? {
        width = value;
    }
    if let Some(value) = env_dimension(&["LINES"])? {
        height = value;
    }
    Ok(Some((width, height)))
}

pub(super) async fn capture(session: &Session) -> Result<Value> {
    // Capture is only worth anything if the result loads again, so a name
    // load would refuse is refused here, before a file is written.
    let name = session.name().to_string_lossy().into_owned();
    if let Some(separator) = normalize::unaddressable(&name) {
        return Err(CliError::invalid(format!(
            "session {name:?} cannot be captured: its name contains {separator:?}, which tmux reads as a target separator, so the workspace could not be loaded back"
        )));
    }
    let workspace = tmux_workspace::freeze(session).await?;
    let mut value = document::parse(&workspace.to_yaml())?;
    let windows = session.windows().await?;
    for (record, window) in value["windows"]
        .as_array_mut()
        .into_iter()
        .flatten()
        .zip(windows)
    {
        record["window_index"] = json!(window.index());
        let options = window
            .options()
            .await?
            .into_iter()
            .map(|(name, value)| (name, option_value(value)))
            .collect::<serde_json::Map<_, _>>();
        if !options.is_empty() {
            record["options_after"] = json!(options);
        }
    }
    let options = session
        .options()
        .await?
        .into_iter()
        // The capturing terminal's size, not anything the workspace
        // asked for; reloading it would pin every future session to it.
        .filter(|(name, _)| name != "default-size")
        .map(|(name, value)| (name, option_value(value)))
        .collect::<serde_json::Map<_, _>>();
    if !options.is_empty() {
        value["options"] = json!(options);
    }
    // Not session.environment_all(): a session inherits the caller's whole
    // process environment, indistinguishable from what the workspace set.
    Ok(value)
}

fn option_value(value: libtmux::OptionValue) -> Value {
    match value {
        libtmux::OptionValue::Flag(value) => json!(value),
        libtmux::OptionValue::Number(value) => json!(value),
        libtmux::OptionValue::Text(value) => json!(value.to_string_lossy()),
        _ => Value::Null,
    }
}

pub(super) async fn freeze(args: &ArgMatches, report: &Reporter) -> Result<()> {
    let server = server(args)?;
    let session =
        selected_session(&server, option(args, "session_name").map(String::as_str)).await?;
    let value = capture(&session).await?;
    let format = option(args, "workspace-format").map_or("yaml", String::as_str);
    // A destination is never derived from the captured session: tmux lets a
    // session name hold path separators and traversal, and nobody typed that
    // name as a path.
    let destination = match option(args, "save-to") {
        Some(path) => Some(PathBuf::from(path)),
        None if report.machine() => None,
        None => {
            return Err(CliError::usage(
                "specify --save-to, or choose --json or --ndjson to capture to stdout",
            ));
        }
    };
    let warnings = [
        "Capture retains live topology, paths, current commands and stored options; original scripts, plugin intent and command history are not recoverable. Invalid UTF-8 is replaced with U+FFFD.",
    ];
    if let Some(path) = destination {
        // `--save-to` names the destination outright; that is consent to
        // write it, with or without a terminal. `--force` still governs
        // replacing a file that is already there.
        document::save(&path, &value, format, flag(args, "force"))?;
        if report.machine() {
            report.document(&json!({"schema_version":1,"command":"freeze","status":"ok","destination":discovery::masked(&path),"format":format,"warnings":warnings}))?;
        } else if !flag(args, "quiet") {
            report.line("success", "Saved", &discovery::masked(&path))?;
            report.line("warning", "Capture limitations", warnings[0])?;
        }
    } else if report.mode == Mode::Ndjson {
        report.document(&json!({"schema_version":1,"command":"freeze","status":"ok","workspace":value,"warnings":warnings}))?;
    } else {
        report.document(&value)?;
    }
    Ok(())
}

fn require_attach_terminal() -> Result<()> {
    if !std::io::stdin().is_terminal() {
        return Err(CliError::usage("attaching requires a terminal; use -d"));
    }
    Ok(())
}

fn unusable_context(detail: &str) -> CliError {
    CliError::usage(format!("{detail}; use -d to load without attaching"))
}

/// Whether an attached load will switch a client or attach one.
///
/// Inside tmux on the server it targets the load ends by switching the
/// client, which needs no terminal but does need the context it was invoked
/// from to still hold: the daemon `TMUX` names is the one answering on that
/// socket, `TMUX_PANE` names a pane there, that pane has a terminal, and a
/// client is attached to its session. Outside tmux the load attaches
/// instead, which needs a terminal. Everything is resolved here, before the
/// load creates anything, so a context that cannot be honoured leaves no
/// session behind.
async fn require_attach_context(server: &Server) -> Result<bool> {
    let context = std::env::var("TMUX").unwrap_or_default();
    if context.is_empty() {
        require_attach_terminal()?;
        return Ok(false);
    }
    let (socket, daemon) = tmux_context(&context)?;
    if !same_socket(std::path::Path::new(socket), server.socket_path()) {
        return Err(unusable_context(
            "the target tmux server is not the current pane's",
        ));
    }
    // A key binding's `run-shell` inherits TMUX without TMUX_PANE: tmux then
    // switches the client that pressed the key, and there is no pane to
    // resolve.
    let pane = std::env::var("TMUX_PANE").unwrap_or_default();
    let mut command = Command::new("display-message").arg("-p");
    command = if pane.is_empty() {
        command.arg("#{pid}")
    } else {
        let pane = pane
            .parse::<libtmux::PaneId>()
            .map_err(|_| unusable_context("TMUX_PANE does not name a pane"))?;
        // A target tmux cannot resolve is not a refusal: it answers with the
        // fields it could fill and leaves the rest empty, so the pane's own
        // id is what says whether it is there.
        command
            .arg("-t")
            .arg(pane.as_ref())
            .arg("#{pid}:#{pane_id}:#{pane_tty}:#{session_attached}")
    };
    let result = server
        .cmd(command)
        .await
        .map_err(|_| unusable_context("the tmux server TMUX names is not running"))?;
    if !result.success() {
        return Err(unusable_context(
            "the tmux server TMUX names is not running",
        ));
    }
    let answer = result.stdout_lossy();
    let mut fields = answer.trim().split(':');
    if fields.next().and_then(|pid| pid.parse::<u32>().ok()) != Some(daemon) {
        return Err(unusable_context(
            "TMUX names a tmux server that is no longer the one on that socket",
        ));
    }
    if pane.is_empty() {
        return Ok(true);
    }
    if fields.next().is_none_or(str::is_empty) {
        return Err(unusable_context(
            "TMUX_PANE does not name a pane on the target tmux server",
        ));
    }
    if !fields.next().is_some_and(|tty| tty.starts_with('/')) {
        return Err(unusable_context("the current pane has no terminal"));
    }
    if fields.next().is_none_or(|count| count == "0") {
        return Err(unusable_context(
            "no tmux client is attached to the current pane's session",
        ));
    }
    Ok(true)
}

/// Compares two socket paths without running a tmux command: lexically
/// first, then resolved, so a symlinked alias for the same server still
/// matches.
fn same_socket(a: &std::path::Path, b: &std::path::Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(a), Ok(b)) if a == b)
}

async fn attach(server: &Server, session: &Session, inside_tmux: bool) -> Result<()> {
    let action = if inside_tmux {
        "switch-client"
    } else {
        "attach-session"
    };
    let mut command = tokio::process::Command::new(server.tmux_executable());
    command.arg("-S").arg(server.socket_path());
    // The handoff is the same tmux the rest of the load talked to, so it is
    // given the same endpoint flags: a client that ignored -2 or -f would
    // come up with different colours or a different configuration.
    if let Some(path) = server.config_file() {
        command.arg("-f").arg(path);
    }
    if server.colors() == Some(256) {
        command.arg("-2");
    }
    let status = command
        .arg(action)
        .arg("-t")
        .arg(session.id().to_string())
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::new(
            "tmux_failed",
            if inside_tmux {
                "the tmux client could not be switched to the session"
            } else {
                "the session could not be attached to this terminal"
            },
        ))
    }
}
