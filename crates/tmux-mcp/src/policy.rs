use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use libtmux::Server;
use libtmux::plan::{OperationKind, Safety as PlanSafety};
use rmcp::model::ErrorData;
use rmcp::schemars;
use serde::Deserialize;

use crate::jobs::Jobs;
use crate::tail::Tails;
use crate::{CallerIdentity, TmuxTools, prompts, schema, tools};

/// The environment variable naming how much of the surface is offered.
pub const SAFETY_ENV: &str = "TMUX_MCP_SAFETY";

/// The environment variable asking before dedicated kills and destructive plans.
pub const CONFIRM_ENV: &str = "TMUX_MCP_CONFIRM";

/// An advertised tool-surface tier.
///
/// A tier filters routes and plan operations. It is not an authorization
/// boundary: open-ended tools can run or type commands whose effects exceed
/// their route class.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Safety {
    /// Read-only routes and read-only plan operations.
    ReadOnly,
    /// Every route except dedicated kill tools, plus non-destructive plans.
    ///
    /// The default. This includes open-ended command and terminal tools, so it
    /// can still produce destructive effects indirectly.
    #[default]
    Mutating,
    /// Every tool and plan operation.
    Destructive,
}

impl Safety {
    /// Read the tier from the environment.
    ///
    /// An absent setting selects the default. An unreadable or unrecognised
    /// setting selects [`Safety::ReadOnly`], so a typo cannot widen the
    /// advertised surface.
    #[must_use]
    pub fn from_env() -> Self {
        safety_from_value(std::env::var(SAFETY_ENV))
    }

    /// Read a tier by name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "readonly" | "read-only" => Some(Self::ReadOnly),
            "mutating" => Some(Self::Mutating),
            "destructive" => Some(Self::Destructive),
            _ => None,
        }
    }

    /// The name this tier is set by.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ReadOnly => "readonly",
            Self::Mutating => "mutating",
            Self::Destructive => "destructive",
        }
    }

    /// Whether a route is offered at this tier.
    ///
    /// MCP annotations describe effects to clients; they are not capability
    /// classes. In particular, `destructiveHint` cannot distinguish an
    /// open-ended shell route from a dedicated kill route.
    fn admits(self, tool: &rmcp::model::Tool) -> bool {
        match (self, route_class(tool.name.as_ref())) {
            (Self::ReadOnly, RouteClass::ReadOnly | RouteClass::Plan)
            | (Self::Mutating, RouteClass::ReadOnly | RouteClass::Mutating | RouteClass::Plan)
            | (Self::Destructive, _) => true,
            (Self::ReadOnly | Self::Mutating, _) => false,
        }
    }

    /// Whether this tier carries the tools a prompt asks the client to use.
    fn admits_prompt(self, name: &str) -> bool {
        match self {
            Self::ReadOnly => name == "diagnose_pane",
            Self::Mutating => matches!(
                name,
                "diagnose_pane" | "run_and_wait" | "interrupt_gracefully"
            ),
            Self::Destructive => true,
        }
    }

    /// Describe the most dangerous plan this tier can admit.
    fn annotate_plan(self, tool: &mut rmcp::model::Tool) {
        let hints = tool.annotations.get_or_insert_default();
        hints.read_only_hint = Some(matches!(self, Self::ReadOnly));
        hints.destructive_hint = Some(!matches!(self, Self::ReadOnly));
        hints.idempotent_hint = Some(matches!(self, Self::ReadOnly));
        hints.open_world_hint = Some(!matches!(self, Self::ReadOnly));
    }

    fn filter_plan_schema(self, tool: &mut rmcp::model::Tool) -> bool {
        schema::retain_tagged_union_variants(
            Arc::make_mut(&mut tool.input_schema),
            "Op",
            |wire_name| {
                OperationKind::from_wire_name(wire_name)
                    .is_some_and(|kind| self.admits_operation(kind.safety()))
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RouteClass {
    ReadOnly,
    Mutating,
    DedicatedKill,
    Plan,
    Unknown,
}

fn route_class(name: &str) -> RouteClass {
    match name {
        "list_sessions"
        | "list_windows"
        | "list_panes"
        | "describe"
        | "list_session_windows"
        | "list_window_panes"
        | "capture_pane"
        | "snapshot_pane"
        | "search_panes"
        | "find_panes"
        | "find_sessions"
        | "job_status"
        | "list_jobs"
        | "list_servers"
        | "show_environment"
        | "show_hooks"
        | "what_changed" => RouteClass::ReadOnly,
        "expand_format" | "new_window" | "rename" | "create_session" | "split_pane"
        | "resize_pane" | "send_keys" | "select_pane" | "select_window" | "run_command"
        | "start_command" | "forget_job" | "show_option" | "set_option" | "set_environment"
        | "pipe_pane" | "select_layout" | "clear_pane" | "respawn_pane" | "paste_text"
        | "signal_channel" | "wait_for_channel" | "capture_since" | "watch_pane"
        | "wait_for_text" | "wait_for_idle" => RouteClass::Mutating,
        "kill_pane" | "kill_window" | "kill_session" | "kill_server" => RouteClass::DedicatedKill,
        "run_plan" => RouteClass::Plan,
        // An unclassified future route is withheld by both ordinary tiers.
        // The exact-surface protocol test requires its author to classify it.
        _ => RouteClass::Unknown,
    }
}

impl Safety {
    /// Whether this tier admits one plan operation.
    ///
    /// Plans carry libtmux's internal operation class. MCP annotations describe
    /// the route as a whole and do not decide this gate.
    pub(super) const fn admits_operation(self, safety: PlanSafety) -> bool {
        match self {
            Self::ReadOnly => matches!(safety, PlanSafety::ReadOnly),
            Self::Mutating => !matches!(safety, PlanSafety::Destructive),
            Self::Destructive => true,
        }
    }
}

/// Assembles a [`TmuxTools`] with the parts the environment usually supplies.
#[derive(Debug)]
pub struct Builder {
    server: Server,
    caller: Option<CallerIdentity>,
    safety: Safety,
    confirm: bool,
}

impl Builder {
    /// Say where this process is running, rather than reading the environment.
    #[must_use]
    pub fn caller(mut self, caller: Option<CallerIdentity>) -> Self {
        self.caller = caller;
        self
    }

    /// Choose the advertised surface, rather than reading the environment.
    ///
    /// This filters routes; it does not confine what an open-ended route can
    /// do with caller-supplied commands or terminal input.
    #[must_use]
    pub const fn safety(mut self, safety: Safety) -> Self {
        self.safety = safety;
        self
    }

    /// Ask before dedicated kills and destructive plans.
    #[must_use]
    pub const fn confirm(mut self, confirm: bool) -> Self {
        self.confirm = confirm;
        self
    }

    /// Build the server, offering only the tools the tier admits.
    #[must_use]
    pub fn build(self) -> TmuxTools {
        let identity = Arc::new(crate::identity::InstanceIdentity::new());
        let mut router = tools::router();
        let plan_schema_valid = if let Some(route) = router.map.get_mut("run_plan") {
            self.safety.annotate_plan(&mut route.attr);
            self.safety.filter_plan_schema(&mut route.attr)
        } else {
            true
        };
        if !plan_schema_valid {
            router.remove_route("run_plan");
        }
        let withheld: Vec<String> = router
            .list_all()
            .iter()
            .filter(|tool| !self.safety.admits(tool))
            .map(|tool| tool.name.to_string())
            .collect();
        for name in withheld {
            router.remove_route(&name);
        }
        for route in router.map.values_mut() {
            schema::strip_unknown_formats(Arc::make_mut(&mut route.attr.input_schema));
            if let Some(schema) = route.attr.output_schema.as_mut() {
                schema::strip_unknown_formats(Arc::make_mut(schema));
            }
        }
        let mut prompt_router = prompts::router();
        let withheld: Vec<String> = prompt_router
            .map
            .keys()
            .filter(|name| !self.safety.admits_prompt(name))
            .map(ToString::to_string)
            .collect();
        for name in withheld {
            prompt_router.remove_route(&name);
        }

        TmuxTools {
            server: Arc::new(self.server),
            caller: self.caller.map(Arc::new),
            safety: self.safety,
            confirm: self.confirm,
            socket: Arc::new(OnceLock::new()),
            tails: Arc::new(Tails::new(Arc::clone(&identity))),
            jobs: Arc::new(Jobs::new(identity)),
            tool_router: router,
            prompt_router,
        }
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

/// Read whether to ask before dedicated kills and destructive plans.
///
/// An absent setting or an explicit no leaves it off. An unreadable or
/// unrecognised setting enables it, so a typo cannot silently remove a gate
/// the operator intended to use.
#[must_use]
pub fn confirm_from_env() -> bool {
    confirm_from_value(std::env::var(CONFIRM_ENV))
}

fn safety_from_value(value: Result<String, std::env::VarError>) -> Safety {
    match value {
        Err(std::env::VarError::NotPresent) => Safety::default(),
        Ok(value) => Safety::parse(&value).unwrap_or(Safety::ReadOnly),
        Err(std::env::VarError::NotUnicode(_)) => Safety::ReadOnly,
    }
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
    /// Ask before a dedicated kill or destructive plan, when configured.
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
    /// tier cannot set one without disturbing every other test. This is how it
    /// says so instead.
    #[must_use]
    pub fn builder(server: Server) -> Builder {
        Builder {
            server,
            caller: CallerIdentity::from_env(),
            safety: Safety::from_env(),
            confirm: confirm_from_env(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::env::VarError;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;
    use std::sync::Arc;

    use rmcp::model::Tool;

    use super::{Safety, confirm_from_value, safety_from_value};

    #[test]
    fn an_unclassified_route_is_withheld_by_default() {
        let tool = Tool::new(
            "future_route",
            "test route",
            Arc::new(serde_json::Map::default()),
        );

        assert!(!Safety::ReadOnly.admits(&tool));
        assert!(!Safety::Mutating.admits(&tool));
        assert!(Safety::Destructive.admits(&tool));
    }

    #[test]
    fn invalid_safety_configuration_closes_the_surface() {
        assert_eq!(
            safety_from_value(Err(VarError::NotPresent)),
            Safety::Mutating
        );
        assert_eq!(
            safety_from_value(Ok("readonl".to_owned())),
            Safety::ReadOnly
        );
        assert_eq!(
            safety_from_value(Err(VarError::NotUnicode(OsString::from_vec(vec![0xff])))),
            Safety::ReadOnly,
        );
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
