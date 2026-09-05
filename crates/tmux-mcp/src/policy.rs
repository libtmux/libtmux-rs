use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use libtmux::Server;
use rmcp::model::ErrorData;
use rmcp::schemars;
use serde::{Deserialize, Serialize};

use crate::jobs::Jobs;
use crate::tail::Tails;
use crate::{CallerIdentity, TmuxTools, schema, tools};

/// The older Rust-specific safety setting, rejected rather than ignored.
pub const RETIRED_RUST_SAFETY_ENV: &str = "TMUX_MCP_SAFETY";

/// The unordered toolsets enabled for this process.
pub const TOOLSETS_ENV: &str = "LIBTMUX_TOOLSETS";

/// Individual tools added after toolset expansion.
pub const TOOLS_ENV: &str = "LIBTMUX_TOOLS";

/// Individual tools removed after every inclusion path.
pub const EXCLUDE_TOOLS_ENV: &str = "LIBTMUX_EXCLUDE_TOOLS";

/// The retired ordered-safety setting, rejected rather than ignored.
pub const RETIRED_SAFETY_ENV: &str = "LIBTMUX_SAFETY";

/// The environment variable asking before teardown tools.
pub const CONFIRM_ENV: &str = "TMUX_MCP_CONFIRM";

/// One mechanical group in the advertised MCP tool inventory.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Toolset {
    /// Read tmux metadata, pane output, environment, or configuration.
    Inspect,
    /// Change tmux state without supplying executable input.
    Manage,
    /// Start configured processes or supply pane input and commands.
    Execute,
    /// Delete tmux state.
    Teardown,
}

impl Toolset {
    const ALL: [Self; 4] = [Self::Inspect, Self::Manage, Self::Execute, Self::Teardown];

    fn parse(name: &str) -> Option<Self> {
        match name {
            "inspect" => Some(Self::Inspect),
            "manage" => Some(Self::Manage),
            "execute" => Some(Self::Execute),
            "teardown" => Some(Self::Teardown),
            _ => None,
        }
    }

    /// The name used in environment selections and capability reports.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Inspect => "inspect",
            Self::Manage => "manage",
            Self::Execute => "execute",
            Self::Teardown => "teardown",
        }
    }
}

/// The startup-frozen request for one MCP tool surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Selection {
    toolsets: Vec<Toolset>,
    include: BTreeSet<String>,
    exclude: BTreeSet<String>,
}

impl Selection {
    /// Parse the three list settings before any tmux connection is opened.
    ///
    /// # Errors
    ///
    /// Returns an error for empty tokens or unknown tool and toolset names.
    pub fn parse(
        toolsets: Option<&str>,
        include: Option<&str>,
        exclude: Option<&str>,
    ) -> Result<Self, SurfaceError> {
        Self::parse_for_socket(toolsets, include, exclude, false)
    }

    /// Parse a selection while applying the selected socket's provenance.
    ///
    /// # Errors
    ///
    /// Returns an error for empty tokens or unknown tool and toolset names.
    pub fn parse_for_socket(
        toolsets: Option<&str>,
        include: Option<&str>,
        exclude: Option<&str>,
        default_teardown: bool,
    ) -> Result<Self, SurfaceError> {
        let toolsets = match toolsets {
            None if default_teardown => Toolset::ALL.to_vec(),
            None => vec![Toolset::Inspect, Toolset::Manage, Toolset::Execute],
            Some("") => Vec::new(),
            Some(value) => parse_names(value, "LIBTMUX_TOOLSETS")?
                .into_iter()
                .map(|name| {
                    Toolset::parse(&name).ok_or_else(|| {
                        SurfaceError::new(format!(
                            "unknown toolset {name:?}; expected inspect, manage, execute, or teardown"
                        ))
                    })
                })
                .collect::<Result<BTreeSet<_>, _>>()?
                .into_iter()
                .collect(),
        };
        Ok(Self {
            toolsets,
            include: parse_optional_names(include, "LIBTMUX_TOOLS")?,
            exclude: parse_optional_names(exclude, "LIBTMUX_EXCLUDE_TOOLS")?,
        })
    }

    /// Read and validate the process-wide selection before serving MCP.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid UTF-8, malformed selections, or retired settings.
    pub fn from_env(default_teardown: bool) -> Result<Self, SurfaceError> {
        for retired in [RETIRED_SAFETY_ENV, RETIRED_RUST_SAFETY_ENV] {
            if std::env::var_os(retired).is_some() {
                return Err(SurfaceError::new(format!(
                    "{retired} has been removed; use {TOOLSETS_ENV}"
                )));
            }
        }
        let toolsets = unicode_env(TOOLSETS_ENV)?;
        let include = unicode_env(TOOLS_ENV)?;
        let exclude = unicode_env(EXCLUDE_TOOLS_ENV)?;
        Self::parse_for_socket(
            toolsets.as_deref(),
            include.as_deref(),
            exclude.as_deref(),
            default_teardown,
        )
    }

    #[must_use]
    /// The startup-frozen toolsets in deterministic order.
    pub fn toolsets(&self) -> &[Toolset] {
        &self.toolsets
    }

    pub(super) fn includes(&self, name: &str) -> bool {
        self.include.contains(name)
    }

    pub(super) fn excludes(&self, name: &str) -> bool {
        self.exclude.contains(name)
    }

    pub(super) fn included_names(&self) -> &BTreeSet<String> {
        &self.include
    }

    pub(super) fn excluded_names(&self) -> &BTreeSet<String> {
        &self.exclude
    }
}

fn unicode_env(name: &'static str) -> Result<Option<String>, SurfaceError> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(value)) => Err(non_unicode(name, value)),
    }
}

fn non_unicode(name: &'static str, _value: OsString) -> SurfaceError {
    SurfaceError::new(format!("{name} must be valid UTF-8"))
}

fn parse_optional_names(
    value: Option<&str>,
    variable: &'static str,
) -> Result<BTreeSet<String>, SurfaceError> {
    match value {
        None | Some("") => Ok(BTreeSet::new()),
        Some(value) => Ok(parse_names(value, variable)?.into_iter().collect()),
    }
}

fn parse_names(value: &str, variable: &'static str) -> Result<Vec<String>, SurfaceError> {
    value
        .split(',')
        .map(|raw| {
            let name = raw.trim();
            if name.is_empty() {
                Err(SurfaceError::new(format!(
                    "{variable} contains an empty name"
                )))
            } else {
                Ok(name.to_owned())
            }
        })
        .collect()
}

/// A startup configuration or manifest error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SurfaceError(String);

impl SurfaceError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for SurfaceError {}

/// What startup can honestly claim about the selected tmux daemon.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SocketProvenance {
    /// A product socket this process can create with minimal configuration.
    DedicatedMinimal,
    /// The product socket already existed, so its configuration is unknown.
    DedicatedExisting,
    /// The operator selected a socket or configuration.
    UserConfigured,
    /// A programmatic builder did not provide provenance.
    #[default]
    Unknown,
}

impl SocketProvenance {
    /// Whether absent toolset configuration may include teardown.
    #[must_use]
    pub const fn defaults_to_teardown(self) -> bool {
        matches!(self, Self::DedicatedMinimal)
    }

    const fn has_minimal_config(self) -> bool {
        matches!(self, Self::DedicatedMinimal)
    }
}

/// Assembles a [`TmuxTools`] with the parts the environment usually supplies.
#[derive(Debug)]
pub struct Builder {
    server: Server,
    caller: Option<CallerIdentity>,
    selection: Selection,
    socket_provenance: SocketProvenance,
    confirm: bool,
}

impl Builder {
    /// Say where this process is running, rather than reading the environment.
    #[must_use]
    pub fn caller(mut self, caller: Option<CallerIdentity>) -> Self {
        self.caller = caller;
        self
    }

    /// Choose the startup-frozen unordered tool surface.
    #[must_use]
    pub fn selection(mut self, selection: Selection) -> Self {
        self.selection = selection;
        self
    }

    /// Record only the socket/configuration provenance startup established.
    #[must_use]
    pub const fn socket_provenance(mut self, provenance: SocketProvenance) -> Self {
        self.socket_provenance = provenance;
        self
    }

    /// Ask before teardown tools.
    #[must_use]
    pub const fn confirm(mut self, confirm: bool) -> Self {
        self.confirm = confirm;
        self
    }

    /// Build the server with its startup-frozen tool selection.
    ///
    /// # Panics
    ///
    /// Panics if native tool metadata violates the capability contract.
    #[must_use]
    #[allow(
        clippy::expect_used,
        reason = "build preserves the existing infallible constructor contract"
    )]
    pub fn build(self) -> TmuxTools {
        self.try_build()
            .expect("native tool routes and the requested surface are valid")
    }

    /// Build the server after validating every named tool against the manifest.
    ///
    /// # Errors
    ///
    /// Returns an error if the native routes or requested selection are invalid.
    pub fn try_build(self) -> Result<TmuxTools, SurfaceError> {
        let identity = Arc::new(crate::identity::InstanceIdentity::new());
        let mut router = tools::router();
        for route in router.map.values_mut() {
            schema::strip_unknown_formats(Arc::make_mut(&mut route.attr.input_schema));
            if let Some(schema) = route.attr.output_schema.as_mut() {
                schema::strip_unknown_formats(Arc::make_mut(schema));
            }
        }
        let mut resolved = crate::manifest::resolve(router, &self.selection)?;
        resolved.report.selected_socket = self.server.socket_path().display().to_string();
        resolved.report.socket_provenance = self.socket_provenance;
        resolved.report.minimal_config_provenance = self.socket_provenance.has_minimal_config();
        let router = resolved.router;
        Ok(TmuxTools {
            server: Arc::new(self.server),
            caller: self.caller.map(Arc::new),
            capability_report: Arc::new(resolved.report),
            confirm: self.confirm,
            socket: Arc::new(OnceLock::new()),
            tails: Arc::new(Tails::new(Arc::clone(&identity))),
            jobs: Arc::new(Jobs::new(identity)),
            tool_router: router,
        })
    }
}

/// Reports how a long call is getting on, when the client asked to be told.
///
/// MCP sends progress only to a request that carried a `progressToken`, so a
/// client that did not ask pays nothing: there is no token, and the notifier
/// does not exist. Without this a sixty-second wait is indistinguishable from
/// a server that has stopped answering.
#[derive(Clone, Debug)]
struct Progress {
    peer: rmcp::service::Peer<rmcp::RoleServer>,
    token: rmcp::model::ProgressToken,
}

/// Whoever asked to be told how a long call is getting on.
///
/// Extracted from the request rather than passed, so a tool declares that it
/// reports progress by taking one. It is empty unless the client sent a
/// progress token, and an empty one can be built directly -- which is what
/// lets these tools be driven without a live client.
#[derive(Clone, Debug, Default)]
pub struct Reporter(Option<Progress>);

impl Reporter {
    /// A reporter with nobody to report to.
    #[must_use]
    pub const fn none() -> Self {
        Self(None)
    }
}

impl<C> rmcp::handler::server::common::FromContextPart<C> for Reporter
where
    C: rmcp::handler::server::common::AsRequestContext,
{
    fn from_context_part(context: &mut C) -> Result<Self, ErrorData> {
        let context = context.as_request_context();
        Ok(Self(context.meta.get_progress_token().map(|token| {
            Progress {
                peer: context.peer.clone(),
                token,
            }
        })))
    }
}

impl Progress {
    /// Say what is happening now.
    ///
    /// `so_far` is seconds elapsed, because the protocol asks for a number
    /// that rises every time and a wait has no other measure of its own
    /// progress: it does not know how long it will take.
    ///
    /// Best-effort: a client that has gone away is the caller's problem to
    /// notice through its own request, not this notification's to report.
    async fn say(&self, so_far: f64, message: impl Into<String>) {
        let mut param = rmcp::model::ProgressNotificationParam::new(self.token.clone(), so_far);
        param.message = Some(message.into());
        let _ = self.peer.notify_progress(param).await;
    }
}

/// Report progress every so often while a future runs.
///
/// Wraps rather than threads a reporter through each primitive: the useful
/// thing to say about a wait is that it is still waiting, and how long for,
/// which needs nothing from inside it.
pub(super) async fn reporting<T>(
    reporter: Reporter,
    what: &str,
    work: impl Future<Output = T>,
) -> T {
    let Some(progress) = reporter.0 else {
        return work.await;
    };

    let began = tokio::time::Instant::now();
    let ticker = async {
        let mut every = tokio::time::interval(PROGRESS_EVERY);
        // The first tick is immediate, and "0 seconds in" says nothing.
        every.tick().await;
        loop {
            every.tick().await;
            let elapsed = began.elapsed().as_secs();
            progress
                .say(
                    f64::from(u32::try_from(elapsed).unwrap_or(u32::MAX)),
                    format!("{what}, {elapsed}s so far"),
                )
                .await;
        }
    };

    tokio::select! {
        outcome = work => outcome,
        () = ticker => unreachable!("the ticker loops forever"),
    }
}

/// How often a long call says it is still going.
const PROGRESS_EVERY: Duration = Duration::from_secs(5);

/// What a person is asked before something irreversible happens.
///
/// One field, because the question is one question. A client renders this
/// from the schema, so the doc comment below is what the person reads.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct Confirmation {
    /// Destroy it?
    pub confirmed: bool,
}

rmcp::elicit_safe!(Confirmation);

/// Whoever can be asked before work is destroyed.
///
/// Extracted like [`Reporter`], and for the same reason: a tool declares that
/// it asks by taking one, and an empty one can be built directly so these
/// tools can still be driven without a live client.
#[derive(Clone, Debug, Default)]
pub struct Asking(Option<rmcp::service::Peer<rmcp::RoleServer>>);

impl Asking {
    /// A gate with nobody to ask.
    #[must_use]
    pub const fn nobody() -> Self {
        Self(None)
    }
}

impl<C> rmcp::handler::server::common::FromContextPart<C> for Asking
where
    C: rmcp::handler::server::common::AsRequestContext,
{
    fn from_context_part(context: &mut C) -> Result<Self, ErrorData> {
        Ok(Self(Some(context.as_request_context().peer.clone())))
    }
}

/// How long a person is given to answer before the question is withdrawn.
///
/// Generous, because the answer is a person leaving their terminal to read a
/// prompt, and short enough that an abandoned client does not hold a tool
/// call open indefinitely.
const CONFIRM_WITHIN: Duration = Duration::from_secs(120);

/// Read whether to ask before teardown tools.
///
/// An absent setting or an explicit no leaves it off. An unreadable or
/// unrecognised setting enables it, so a typo cannot silently remove a gate
/// the operator intended to use.
#[must_use]
pub fn confirm_from_env() -> bool {
    confirm_from_value(std::env::var(CONFIRM_ENV))
}

fn confirm_from_value(value: Result<String, std::env::VarError>) -> bool {
    match value {
        Err(std::env::VarError::NotPresent) => false,
        Err(std::env::VarError::NotUnicode(_)) => true,
        Ok(value) => !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        ),
    }
}

impl TmuxTools {
    /// Ask before a teardown operation, when configured.
    ///
    /// Fails closed. A server told to confirm and given no way to ask has to
    /// refuse: proceeding would be exactly the unattended destruction the
    /// setting exists to prevent, and the operator would never learn the
    /// question went unasked.
    pub(super) async fn permitted(&self, asking: &Asking, what: &str) -> Result<(), ErrorData> {
        if !self.confirm {
            return Ok(());
        }

        let Some(peer) = asking.0.as_ref() else {
            return Err(refused_without_asking(what, "there is no client to ask"));
        };

        match peer
            .elicit_with_timeout::<Confirmation>(
                format!("Destroy {what}? This cannot be undone."),
                Some(CONFIRM_WITHIN),
            )
            .await
        {
            Ok(Some(answer)) if answer.confirmed => Ok(()),
            Ok(Some(_)) => Err(refused_without_asking(what, "the request was declined")),
            Ok(None) => Err(refused_without_asking(what, "the request was dismissed")),
            Err(error) => Err(refused_without_asking(
                what,
                &format!("the client could not ask: {error}"),
            )),
        }
    }
}

/// Report a destructive call that was not approved.
///
/// Classified `refused` rather than `object_gone`: nothing is stale, and the
/// answer is not to look again but to get a person to agree.
fn refused_without_asking(what: &str, why: &str) -> ErrorData {
    let mut data = serde_json::Map::new();
    data.insert("kind".into(), "refused".into());
    data.insert("retryable".into(), false.into());
    data.insert("stale".into(), false.into());

    ErrorData::new(
        rmcp::model::ErrorCode::INVALID_REQUEST,
        format!("destroying {what} was not approved: {why}"),
        Some(serde_json::Value::Object(data)),
    )
}

impl TmuxTools {
    /// Expose one tmux server, locating this process within it.
    #[must_use]
    pub fn new(server: Server) -> Self {
        Self::builder(server).build()
    }

    /// Expose one tmux server, saying explicitly where this process is and how
    /// much of the surface it may use.
    ///
    /// The environment is process-wide, so a test that needs a caller or a
    /// selection cannot set one without disturbing every other test. This is how it
    /// says so instead.
    #[must_use]
    pub fn builder(server: Server) -> Builder {
        Builder {
            server,
            caller: CallerIdentity::from_env(),
            selection: Selection {
                toolsets: vec![Toolset::Inspect, Toolset::Manage, Toolset::Execute],
                include: BTreeSet::new(),
                exclude: BTreeSet::new(),
            },
            socket_provenance: SocketProvenance::Unknown,
            confirm: confirm_from_env(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::env::VarError;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    use super::{Selection, Toolset, confirm_from_value};

    #[test]
    fn toolset_selection_distinguishes_empty_from_empty_tokens() {
        let empty = Selection::parse(Some(""), None, None).expect("empty surface");
        assert!(empty.toolsets().is_empty());

        let inspect = Selection::parse(Some("inspect"), None, None).expect("one toolset");
        assert_eq!(inspect.toolsets(), &[Toolset::Inspect]);

        for malformed in [",inspect", "inspect,", "inspect,,manage"] {
            let error = Selection::parse(Some(malformed), None, None).expect_err("empty token");
            assert!(error.to_string().contains("empty"), "{malformed}: {error}");
        }
    }

    #[test]
    fn registered_routes_are_the_capability_manifest() {
        let selection =
            Selection::parse_for_socket(None, None, None, true).expect("dedicated minimal surface");
        let resolved = crate::manifest::resolve(crate::tools::router(), &selection)
            .expect("complete manifest");
        let listed = resolved.router.list_all();
        let reported: Vec<_> = resolved
            .report
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        let names: Vec<_> = listed.iter().map(|tool| tool.name.as_ref()).collect();

        assert_eq!(names, reported);
        assert!(listed.iter().all(|tool| {
            let description = tool.description.as_deref().expect("description");
            resolved.report.tools.iter().any(|row| {
                row.name == tool.name && description.starts_with(row.controlled_opener())
            })
        }));
    }

    #[test]
    fn native_surface_matches_the_cross_port_contract() {
        let expected: BTreeSet<_> = [
            "list_sessions",
            "list_windows",
            "list_panes",
            "get_server_info",
            "get_session_info",
            "get_window_info",
            "get_pane_info",
            "capture_pane",
            "capture_since",
            "snapshot_pane",
            "search_panes",
            "find_pane_by_position",
            "wait_for_text",
            "get_tmux_variables",
            "show_option",
            "show_environment",
            "show_hooks",
            "call_read_tools_batch",
            "rename_session",
            "rename_window",
            "select_window",
            "select_pane",
            "select_layout",
            "resize_window",
            "resize_pane",
            "move_window",
            "swap_pane",
            "set_pane_title",
            "enter_copy_mode",
            "exit_copy_mode",
            "wait_for_channel",
            "signal_channel",
            "set_mouse_enabled",
            "set_history_limit",
            "create_session",
            "create_window",
            "split_window",
            "respawn_pane",
            "run_shell_command",
            "send_keys",
            "send_keys_batch",
            "paste_text",
            "set_synchronize_panes",
            "clear_pane_scrollback",
            "kill_pane",
            "kill_window",
            "kill_session",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let actual: BTreeSet<_> = crate::tools::router()
            .map
            .keys()
            .map(ToString::to_string)
            .collect();

        assert_eq!(actual, expected);
    }

    #[test]
    fn unknown_provenance_defaults_without_teardown() {
        let selection = Selection::parse(None, None, None).expect("conservative default");
        assert_eq!(
            selection.toolsets(),
            &[Toolset::Inspect, Toolset::Manage, Toolset::Execute]
        );
    }

    #[test]
    fn spawn_routes_accept_no_command_or_environment_payload() {
        let router = crate::tools::router();
        for name in [
            "create_session",
            "create_window",
            "split_window",
            "respawn_pane",
        ] {
            let schema = &router.get(name).expect("spawn route").input_schema;
            let keys: BTreeSet<_> = schema
                .get("properties")
                .and_then(serde_json::Value::as_object)
                .expect("object schema")
                .keys()
                .map(String::as_str)
                .collect();
            assert!(!keys.contains("command"), "{name}");
            assert!(!keys.contains("environment"), "{name}");
            assert!(!keys.contains("env"), "{name}");
        }
    }

    #[test]
    fn exclusion_removes_aggregate_nested_authority() {
        let selection =
            Selection::parse(Some("inspect"), None, Some("capture_pane")).expect("selection");
        let resolved =
            crate::manifest::resolve(crate::tools::router(), &selection).expect("resolved surface");
        let batch = resolved
            .report
            .tools
            .iter()
            .find(|tool| tool.name == "call_read_tools_batch")
            .expect("batch route");

        assert!(!batch.capability.nested_authority.contains("capture_pane"));
    }

    #[test]
    fn read_batch_has_exact_unbounded_inspect_authority() {
        let selection = Selection::parse(Some("inspect"), None, None).expect("selection");
        let resolved =
            crate::manifest::resolve(crate::tools::router(), &selection).expect("resolved surface");
        let batch = resolved
            .report
            .tools
            .iter()
            .find(|tool| tool.name == "call_read_tools_batch")
            .expect("batch route");
        let expected = [
            "list_sessions",
            "list_windows",
            "list_panes",
            "get_server_info",
            "get_session_info",
            "get_window_info",
            "get_pane_info",
            "capture_pane",
            "capture_since",
            "snapshot_pane",
            "search_panes",
            "find_pane_by_position",
            "get_tmux_variables",
            "show_option",
            "show_environment",
            "show_hooks",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();

        assert_eq!(batch.capability.nested_authority, expected);
    }

    #[test]
    fn invalid_confirmation_configuration_enables_the_gate() {
        assert!(!confirm_from_value(Err(VarError::NotPresent)));
        assert!(!confirm_from_value(Ok("false".to_owned())));
        assert!(confirm_from_value(Ok("ture".to_owned())));
        assert!(confirm_from_value(Err(VarError::NotUnicode(
            OsString::from_vec(vec![0xff]),
        ))));
    }
}
