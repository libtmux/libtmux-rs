use std::{fmt::Write as _, path::PathBuf};

use clap::ArgMatches;
use libtmux::{
    Command, NewSessionOptions, NewWindowOptions, Server, Session, SplitDirection, SplitOptions,
    escape_format,
};
use serde_json::{Value, json};

use super::{
    CliError, Result, bridge, confirm, discovery, document, normalize,
    output::{Mode, Reporter},
    process,
};

#[derive(Default)]
struct Effects {
    input: usize,
    session: Option<Session>,
    owned: bool,
    changed: bool,
    mutation_started: Option<&'static str>,
    windows: Vec<String>,
    panes: Vec<String>,
    stage: &'static str,
    script_output: Option<Value>,
    readiness: bool,
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
        }
        json!({"schema_version":1,"command":"load","status":if partial {"partial"} else {"error"},"errors":[failure],"results":self.results})
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
        return server.session(name).await?.ok_or_else(|| {
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
    let mut sessions = server.sessions().await?;
    if sessions.len() == 1 {
        return sessions
            .pop()
            .ok_or_else(|| CliError::new("session_not_found", "no session"));
    }
    Err(CliError::new(
        "session_required",
        "select a session by name",
    ))
}

async fn load_target(args: &ArgMatches) -> Result<(Server, Option<AppendTarget>)> {
    let server = server(args)?;
    let borrowed = if flag(args, "append") {
        Some(append_target(&server).await?)
    } else {
        None
    };
    Ok((server, borrowed))
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
        return Err(CliError {
            code: "unsupported_color_mode",
            message: "tmux 3.2a and newer do not support 88-color mode; omit -8 or use -2".into(),
            status: 2,
            retained_state: None,
        });
    }
    if report.machine() && !flag(args, "detached") && !flag(args, "append") {
        return Err(CliError::usage("machine load requires -d or --append"));
    }
    let workspaces = load_inputs(args)?;
    if !flag(args, "detached") && !flag(args, "append") {
        require_attach_terminal()?;
    }
    report.progress = super::progress::Progress::new(args, report.machine())?;
    if let Some(path) = option(args, "log-file") {
        report
            .log
            .open(std::path::Path::new(&discovery::expand(path)))?;
    }
    let (server, borrowed) = load_target(args).await?;
    server.validate_layouts(native_layouts(&workspaces)).await?;
    let python = if workspaces.iter().any(|(_, workspace)| workspace.bridge) {
        Some(process::python().await?)
    } else {
        None
    };
    let mut last_session = None;
    report.event("started", json!({"inputs":workspaces.len()}))?;
    for (index, (path, workspace)) in workspaces.iter().enumerate() {
        let effects = state.current.insert(Effects {
            input: index,
            ..Effects::default()
        });
        let outcome = async {
            if let Some(progress) = &mut report.progress {
                progress.start(workspace)?;
            }
            report.event(
                "workspace-started",
                json!({"input_index":index,"input":discovery::masked(path)}),
            )?;
            let (session, reused) = build(
                &server,
                workspace,
                args,
                report,
                effects,
                python.as_deref(),
                borrowed.as_ref(),
            )
            .await?;
            let mut result = effects.value();
            result["input"] = json!(discovery::masked(path));
            result["reused"] = json!(reused);
            state.results.push(result.clone());
            last_session = Some(session);
            if let Some(progress) = &mut report.progress {
                progress.finish(reused, workspace.bridge)?;
            }
            report.event("workspace-completed", result)
        }
        .await;
        if let Err(mut error) = outcome {
            let summary = state.failure(&error);
            if let Err(publication) = report.summary("failed", &summary) {
                let _ = write!(error.message, "; output failed: {publication}");
            }
            error.retained_state = Some(summary);
            return Err(error);
        }
    }
    let mut summary = json!({"schema_version":1,"command":"load","status":"ok","errors":[]});
    summary["results"] = state.results.clone().into();
    state.completed = Some(summary.clone());
    let outcome = async {
        report.summary("completed", &summary)?;
        if !report.machine() {
            report.loaded(&summary["results"])?;
            std::io::Write::flush(&mut std::io::stdout())?;
            report.log_warning();
            if !flag(args, "detached") && !flag(args, "append") {
                if let Some(session) = last_session {
                    attach(&server, &session).await?;
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

fn append_context(message: impl Into<String>) -> CliError {
    CliError::new("append_context", message)
}

fn tmux_context(context: &str) -> Result<(&str, u32)> {
    let parsed = context.rsplit_once(',').and_then(|(prefix, session)| {
        session.parse::<u32>().ok()?;
        let (socket, pid) = prefix.rsplit_once(',')?;
        let pid = pid.parse::<u32>().ok()?;
        (!socket.is_empty() && pid > 0).then_some((socket, pid))
    });
    parsed.ok_or_else(|| append_context("TMUX must identify a socket, daemon PID, and session"))
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
        .map_err(|error| append_context(error.to_string()))?;
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
        .map_err(|_| {
            CliError::new(
                "current_pane_required",
                "append requires TMUX_PANE identifying the current pane",
            )
        })?
        .parse::<libtmux::PaneId>()
        .map_err(|error| append_context(error.to_string()))?;
    let context = std::env::var("TMUX")
        .map_err(|_| append_context("append requires the inherited TMUX daemon identity"))?;
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
    server: &Server,
    workspace: &normalize::Workspace,
    args: &ArgMatches,
    report: &mut Reporter,
    effects: &mut Effects,
    python: Option<&std::ffi::OsStr>,
    borrowed: Option<&AppendTarget>,
) -> Result<(Session, bool)> {
    if let Some(target) = borrowed {
        effects.session = Some(target.session.clone());
        effects.stage = "append-validation";
        target.recheck(server).await?;
    }
    if workspace.bridge {
        return build_extension(
            server,
            workspace,
            args,
            report,
            effects,
            python.ok_or_else(|| {
                CliError::new("python_runtime", "checked Python runtime is missing")
            })?,
            borrowed,
        )
        .await;
    }
    let input = effects.input;
    let append = borrowed.is_some();
    if !append && server.is_alive().await {
        if let Some(session) = server.session(&workspace.name).await? {
            effects.session = Some(session.clone());
            effects.stage = "reused";
            return Ok((session, true));
        }
    }
    let session = if let Some(target) = borrowed {
        target.session.clone()
    } else {
        let columns = dimension("TMUXP_DEFAULT_COLUMNS", "COLUMNS", 80);
        let rows = dimension("TMUXP_DEFAULT_ROWS", "ROWS", 24);
        effects.stage = "session-creation";
        effects.mutation_started = Some(effects.stage);
        let session = server
            .new_session(
                NewSessionOptions::new(escape_format(&workspace.name))
                    .start_directory(escape_format(&workspace.directory))
                    .size(columns, rows),
            )
            .await?;
        effects.session = Some(session.clone());
        effects.owned = true;
        effects.changed = true;
        effects.stage = "session-created";
        report.event("session-created", json!({"input_index":input,"session_id":session.id().to_string(),"session_name":workspace.name}))?;
        session
    };
    effects.session = Some(session.clone());
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
        let window = build_window(&session, config, report, effects, window_index).await?;
        if config.focus || selected.is_none() {
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
        let output = process::run(&argv, &workspace.script_directory, report).await?;
        effects.script_output = Some(output.value());
        output.success()?;
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
        server.set_option(name, value).await?;
    }
    effects.readiness = match workspace.readiness {
        Some(wait) => wait,
        None => session
            .get_option("default-shell")
            .await?
            .map_or_else(
                || std::env::var("SHELL").unwrap_or_default(),
                |value| value.to_string_lossy().into_owned(),
            )
            .contains("zsh"),
    };
    Ok(())
}

async fn build_extension(
    server: &Server,
    workspace: &normalize::Workspace,
    args: &ArgMatches,
    report: &mut Reporter,
    effects: &mut Effects,
    python: &std::ffi::OsStr,
    borrowed: Option<&AppendTarget>,
) -> Result<(Session, bool)> {
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
        .map_err(|error| CliError::new("python_extension", error.message))?;
    effects.stage = "completed";
    let session = current.ok_or_else(|| {
        CliError::new(
            "python_extension",
            "Python builder completed without a session",
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
    report.event("window-created", json!({"input_index":input,"window_index":window.index(),"window_id":window.id().to_string()}))?;
    for (name, value) in &config.options {
        window.set_option(name, value).await?;
    }
    let mut panes = vec![
        window
            .active_pane()
            .await?
            .ok_or_else(|| CliError::new("pane_missing", "new window has no pane"))?,
    ];
    effects.panes.push(panes[0].id().to_string());
    // Split the pane the previous split made, not the window: `-t <window>`
    // always resolves to the window's active pane, and a detached split
    // never changes which pane that is, so targeting the window on every
    // iteration keeps dividing pane 0 and pushes each new pane in front of
    // the last (H1). `panes` always starts with the window's first pane, so
    // there is always a source to split from.
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
    let mut active = None;
    for (pane_index, (pane, config)) in panes.iter().zip(&config.panes).enumerate() {
        if let Some(progress) = &mut report.progress {
            progress.pane(pane_index + 1)?;
        }
        report.event("pane-created", json!({"input_index":input,"window_index":window_index,"pane_id":pane.id().to_string()}))?;
        if effects.readiness && config.shell.is_none() {
            wait_for_prompt(pane, report).await?;
        }
        send_commands(pane, &config.commands).await?;
        if let Some(progress) = &mut report.progress {
            progress.pane_done()?;
        }
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

fn dimension(preferred: &str, fallback: &str, default: u32) -> u32 {
    std::env::var(preferred)
        .or_else(|_| std::env::var(fallback))
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

pub(super) async fn capture(session: &Session) -> Result<Value> {
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
        .map(|(name, value)| (name, option_value(value)))
        .collect::<serde_json::Map<_, _>>();
    if !options.is_empty() {
        value["options"] = json!(options);
    }
    let mut environment = serde_json::Map::new();
    for (name, entry) in session.environment_all().await? {
        if let libtmux::EnvironmentEntry::Set(text) = entry {
            environment.insert(name, json!(text.to_string_lossy()));
        }
    }
    if !environment.is_empty() {
        value["environment"] = json!(environment);
    }
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
        if !report.machine() && !flag(args, "yes") {
            confirm(&format!("Save {}?", path.display()))?;
        }
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
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return Err(CliError::new(
            "terminal_required",
            "attaching requires a terminal; use -d",
        ));
    }
    Ok(())
}

async fn attach(server: &Server, session: &Session) -> Result<()> {
    require_attach_terminal()?;
    let action = if std::env::var_os("TMUX").is_some() {
        "switch-client"
    } else {
        "attach-session"
    };
    let status = tokio::process::Command::new(server.tmux_executable())
        .arg("-S")
        .arg(server.socket_path())
        .arg(action)
        .arg("-t")
        .arg(session.id().to_string())
        .status()
        .await?;
    if status.success() {
        Ok(())
    } else {
        Err(CliError::new(
            "attach_failed",
            format!("tmux {action} exited with {status}"),
        ))
    }
}
