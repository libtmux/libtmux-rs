use std::path::PathBuf;

use clap::ArgMatches;
use libtmux::{
    Command, NewSessionOptions, NewWindowOptions, Server, Session, SplitDirection, SplitOptions,
    escape_format,
};
use serde_json::{Value, json};

use super::{
    CliError, Result, confirm, discovery, document, normalize,
    output::{Mode, Reporter},
    process,
};

#[derive(Default)]
struct Effects {
    input: usize,
    session: Option<Session>,
    owned: bool,
    changed: bool,
    windows: Vec<String>,
    panes: Vec<String>,
    stage: &'static str,
    script_output: Option<Value>,
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
        if let Some(socket) = context.split(',').next().filter(|s| !s.is_empty()) {
            builder = builder.socket_path(socket);
        }
    }
    if let Some(config) = option(args, "tmux-config") {
        builder = builder.config_file(config);
    }
    if flag(args, "colors256") {
        builder = builder.colors(256);
    }
    if flag(args, "colors88") {
        builder = builder.colors(88);
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

pub(super) async fn load(args: &ArgMatches, report: &mut Reporter) -> Result<()> {
    if report.machine() && !flag(args, "detached") && !flag(args, "append") {
        return Err(CliError::usage("machine load requires -d or --append"));
    }
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
        if workspace.bridge {
            return Err(CliError::new(
                "python_bridge_required",
                "Python plugins/custom builders require the version-checked tmuxp bridge",
            ));
        }
        workspaces.push((path, workspace));
    }
    let server = server(args)?;
    let mut results = Vec::new();
    let mut last_session = None;
    report.event("started", json!({"inputs":workspaces.len()}))?;
    for (index, (path, workspace)) in workspaces.iter().enumerate() {
        report.event(
            "workspace-started",
            json!({"input_index":index,"input":discovery::masked(path)}),
        )?;
        let mut effects = Effects {
            input: index,
            ..Effects::default()
        };
        match build(&server, workspace, args, report, &mut effects).await {
            Ok((session, reused)) => {
                let mut result = effects.value();
                result["input"] = json!(discovery::masked(path));
                result["reused"] = json!(reused);
                report.event("workspace-completed", result.clone())?;
                results.push(result);
                last_session = Some(session);
            }
            Err(error) => {
                let partial = effects.changed;
                let errors = json!([{"code":error.code,"message":error.message,"input_index":index,"partial_effects":partial,"effects":effects.value()}]);
                if report.mode == Mode::Json {
                    report.document(&json!({"schema_version":1,"command":"load","status":if partial || !results.is_empty(){"partial"}else{"error"},"results":results,"errors":errors}))?;
                }
                report.event("failed", json!({"status":if partial || !results.is_empty(){"partial"}else{"error"},"results":results,"errors":errors}))?;
                return Err(error);
            }
        }
    }
    if report.mode == Mode::Json {
        report.document(&json!({"schema_version":1,"command":"load","status":"ok","results":results,"errors":[]}))?;
    }
    report.event(
        "completed",
        json!({"status":"ok","results":results,"errors":[]}),
    )?;
    if !report.machine() {
        for result in &results {
            report.line(
                "success",
                if result["reused"] == true {
                    "Reused"
                } else {
                    "Loaded"
                },
                result["session_name"].as_str().unwrap_or(""),
            )?;
        }
        if !flag(args, "detached") && !flag(args, "append") {
            if let Some(session) = last_session {
                attach(&server, &session).await?;
            }
        }
    }
    Ok(())
}

async fn current_session(server: &Server) -> Result<Session> {
    let pane = std::env::var("TMUX_PANE").map_err(|_| {
        CliError::new(
            "current_pane_required",
            "append requires TMUX_PANE identifying the current pane on the selected server",
        )
    })?;
    let result = server
        .cmd(
            Command::new("display-message")
                .arg("-p")
                .arg("-t")
                .arg(pane)
                .arg("#{session_id}"),
        )
        .await?;
    let target = result.stdout_lossy().trim().to_owned();
    let id = target
        .parse::<libtmux::SessionId>()
        .map_err(|error| CliError::new("current_pane_required", error.to_string()))?;
    server.session_by_id(&id).await?.ok_or_else(|| {
        CliError::new(
            "current_pane_required",
            "current pane has no session on the selected server",
        )
    })
}

async fn build(
    server: &Server,
    workspace: &normalize::Workspace,
    args: &ArgMatches,
    report: &mut Reporter,
    effects: &mut Effects,
) -> Result<(Session, bool)> {
    let input = effects.input;
    let append = flag(args, "append");
    let existing = if server.is_alive().await {
        server.session(&workspace.name).await?
    } else {
        None
    };
    if !append {
        if let Some(session) = existing {
            effects.session = Some(session.clone());
            effects.stage = "reused";
            return Ok((session, true));
        }
    }
    let session = if append {
        current_session(server).await?
    } else {
        let columns = dimension("TMUXP_DEFAULT_COLUMNS", "COLUMNS", 80);
        let rows = dimension("TMUXP_DEFAULT_ROWS", "ROWS", 24);
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
    if let Some(script) = &workspace.before_script {
        effects.stage = "before-script";
        effects.changed = true;
        let output = process::run(
            &process::split(script)?,
            &workspace.script_directory,
            report,
        )
        .await?;
        effects.script_output = Some(output.value());
        output.success()?;
    }
    effects.stage = "session-options";
    effects.changed |= !workspace.environment.is_empty()
        || !workspace.options.is_empty()
        || !workspace.global_options.is_empty();
    for (name, value) in &workspace.environment {
        session.set_environment(name, value).await?;
    }
    for (name, value) in &workspace.options {
        session.set_option(name, value).await?;
    }
    for (name, value) in &workspace.global_options {
        server.set_option(name, value).await?;
    }
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
        let window = build_window(server, &session, config, report, effects, window_index).await?;
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

async fn build_window(
    server: &Server,
    session: &Session,
    config: &normalize::Window,
    report: &mut Reporter,
    effects: &mut Effects,
    window_index: usize,
) -> Result<libtmux::Window> {
    let input = effects.input;
    effects.stage = "window-creation";
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
    let window = session.new_window(options).await?;
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
    for pane in config.panes.iter().skip(1) {
        let mut split = SplitOptions::new(SplitDirection::Below)
            .start_directory(escape_format(&pane.directory));
        if let Some(shell) = &pane.shell {
            split = split.command(shell);
        }
        for (name, value) in &pane.environment {
            split = split.environment(name, value);
        }
        let pane = window.split(split).await?;
        effects.panes.push(pane.id().to_string());
        panes.push(pane);
        server
            .cmd(
                Command::new("select-layout")
                    .arg("-t")
                    .arg(window.id().to_string())
                    .arg("tiled"),
            )
            .await?;
    }
    if let Some(layout) = &config.layout {
        server
            .cmd(
                Command::new("select-layout")
                    .arg("-t")
                    .arg(window.id().to_string())
                    .arg("--")
                    .arg(layout),
            )
            .await?;
    }
    let mut active = None;
    for (pane, config) in panes.iter().zip(&config.panes) {
        report.event("pane-created", json!({"input_index":input,"window_index":window_index,"pane_id":pane.id().to_string()}))?;
        for command in &config.commands {
            tokio::time::sleep(command.before).await;
            if command.enter {
                pane.send_line(&command.text).await?;
            } else {
                pane.send_keys(&command.text).await?;
            }
            tokio::time::sleep(command.after).await;
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
    Ok(window)
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
    let destination = option(args, "save-to").map(PathBuf::from).or_else(|| {
        (!report.machine())
            .then(|| PathBuf::from(format!("{}.{}", session.name().to_string_lossy(), format)))
    });
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

async fn attach(server: &Server, session: &Session) -> Result<()> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        return Err(CliError::new(
            "terminal_required",
            "attaching requires a terminal; use -d",
        ));
    }
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
