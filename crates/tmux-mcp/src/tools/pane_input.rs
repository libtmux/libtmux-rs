use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use libtmux::{Command, ServerGeneration};
use rmcp::model::ErrorData;

use crate::TmuxTools;
use crate::run_request::{self, PaneReservation};

use super::error::{bad_input, object_gone, tmux_error, vanished};

#[derive(Clone, Copy)]
pub(crate) enum PaneInputReach {
    TargetOnly,
    Synchronized,
}

#[derive(Clone, Copy)]
pub(crate) enum MissingSource {
    CallerInput,
    ObservedTransition,
    PasteTransition,
}

pub(crate) struct PaneInputPlan {
    pub(crate) target: libtmux::Pane,
    pub(crate) configured: Vec<String>,
    pub(crate) endpoint: PathBuf,
    pub(crate) generation: ServerGeneration,
    signature: PaneInputSignature,
}

impl PaneInputPlan {
    pub(crate) fn reserve(&self) -> Option<PaneReservation> {
        run_request::reserve(self.generation, &self.endpoint, &self.configured)
    }

    pub(crate) fn same_authority(&self, other: &Self) -> bool {
        self.target.id() == other.target.id()
            && self.configured == other.configured
            && self.endpoint == other.endpoint
            && self.generation == other.generation
            && self.signature == other.signature
    }

    pub(crate) fn owns(&self, reservation: &PaneReservation) -> bool {
        run_request::owns(
            reservation,
            self.generation,
            &self.endpoint,
            &self.configured,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the signature preserves four independent tmux safety flags"
)]
struct PaneState {
    pane_id: String,
    window_id: String,
    synchronized: bool,
    dead: bool,
    input_disabled: bool,
    in_mode: bool,
    current_command: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PaneMember {
    state: PaneState,
    placements: BTreeSet<WindowPlacement>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct WindowPlacement {
    session_id: String,
    window_id: String,
    window_index: i32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PaneInputSignature {
    source: PaneMember,
    configured: Vec<PaneMember>,
    caller: Option<crate::CallerIdentity>,
    clients: Vec<TerminalClient>,
}

struct PaneSnapshot {
    handles: BTreeMap<String, libtmux::Pane>,
    members: BTreeMap<String, PaneMember>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TerminalClient {
    session: String,
    window: String,
    window_index: i32,
    pane: String,
    zoomed: bool,
}

struct ClientAttention {
    attended: BTreeSet<String>,
    terminals: Vec<TerminalClient>,
}

const CLIENT_ATTENTION_FORMAT: &str = "#{client_control_mode}|#{session_id}|#{window_id}|#{window_index}|#{pane_id}|#{window_zoomed_flag}";

fn client_attention_error(detail: &str) -> ErrorData {
    ErrorData::internal_error(
        format!("tmux returned malformed client attention state: {detail}"),
        Some(serde_json::json!({
            "kind": "decode",
            "retryable": false,
            "stale": false,
        })),
    )
}

fn endpoint_error(message: impl Into<String>) -> ErrorData {
    ErrorData::internal_error(
        message.into(),
        Some(serde_json::json!({
            "kind": "decode",
            "retryable": false,
            "stale": false,
        })),
    )
}

fn pane_snapshot_error(detail: &str) -> ErrorData {
    ErrorData::internal_error(
        format!("tmux returned malformed pane input state: {detail}"),
        Some(serde_json::json!({
            "kind": "decode",
            "retryable": false,
            "stale": false,
        })),
    )
}

fn missing_source_error(pane: &str, missing: MissingSource) -> ErrorData {
    match missing {
        MissingSource::CallerInput => object_gone("pane", pane),
        MissingSource::ObservedTransition => {
            vanished(&format!("pane {pane} disappeared between run checkpoints"))
        }
        MissingSource::PasteTransition => {
            vanished(&format!("pane {pane} disappeared before paste dispatch"))
        }
    }
}

pub(crate) fn active_run_error(pane: &str) -> ErrorData {
    ErrorData::internal_error(
        format!(
            "pane {pane} has an active run_shell_command; wait for its completion or pane closure before sending more input"
        ),
        Some(serde_json::json!({
            "kind": "active_run",
            "retryable": true,
            "stale": false,
        })),
    )
}

fn parse_flag(value: &[u8]) -> Result<bool, &'static str> {
    match value {
        b"0" => Ok(false),
        b"1" => Ok(true),
        _ => Err("a client flag was not exactly 0 or 1"),
    }
}

fn parse_id<T>(value: &[u8]) -> Result<String, &'static str>
where
    T: FromStr + ToString,
{
    let text = std::str::from_utf8(value).map_err(|_| "a client ID was not UTF-8")?;
    let parsed = text.parse::<T>().map_err(|_| "a client ID was invalid")?;
    if parsed.to_string() != text {
        return Err("a client ID was not canonical");
    }
    Ok(text.to_owned())
}

fn parse_window_index(value: &[u8]) -> Result<i32, &'static str> {
    let text = std::str::from_utf8(value).map_err(|_| "a window index was not UTF-8")?;
    let parsed = text
        .parse::<i32>()
        .map_err(|_| "a window index was invalid")?;
    if parsed.to_string() != text {
        return Err("a window index was not canonical");
    }
    Ok(parsed)
}

fn parse_client_attention(
    stdout: &[u8],
    members: &BTreeMap<String, PaneMember>,
    source_window: &str,
) -> Result<ClientAttention, &'static str> {
    if stdout.is_empty() {
        return Ok(ClientAttention {
            attended: BTreeSet::new(),
            terminals: Vec::new(),
        });
    }
    let rows = stdout
        .strip_suffix(b"\n")
        .ok_or("the client listing ended without a row terminator")?;
    let mut attended = BTreeSet::new();
    let mut terminals = Vec::new();
    for row in rows.split(|byte| *byte == b'\n') {
        if row.is_empty() {
            return Err("the client listing contained an empty row");
        }
        let fields = row.split(|byte| *byte == b'|').collect::<Vec<_>>();
        let [control, session, window, window_index, pane, zoomed] = fields.as_slice() else {
            return Err("a client row did not have exactly six fields");
        };
        let control = parse_flag(control)?;
        if control {
            continue;
        }
        let session = parse_id::<libtmux::SessionId>(session)?;
        let window = parse_id::<libtmux::WindowId>(window)?;
        let window_index = parse_window_index(window_index)?;
        let active = parse_id::<libtmux::PaneId>(pane)?;
        let zoomed = parse_flag(zoomed)?;
        let Some(member) = members.get(&active) else {
            return Err("a terminal client pane was absent from the pane snapshot");
        };
        if !member.placements.contains(&WindowPlacement {
            session_id: session.clone(),
            window_id: window.clone(),
            window_index,
        }) {
            return Err("a terminal client reported inconsistent pane placement");
        }
        terminals.push(TerminalClient {
            session,
            window: window.clone(),
            window_index,
            pane: active.clone(),
            zoomed,
        });
        if window != source_window {
            continue;
        }
        if zoomed {
            attended.insert(active);
        } else {
            attended.extend(
                members
                    .values()
                    .filter(|candidate| candidate.state.window_id == window)
                    .map(|candidate| candidate.state.pane_id.clone()),
            );
        }
    }
    terminals.sort_unstable();
    Ok(ClientAttention {
        attended,
        terminals,
    })
}

fn merge_member(
    members: &mut BTreeMap<String, PaneMember>,
    state: PaneState,
    session: String,
    window_index: i32,
) -> Result<bool, &'static str> {
    let placement = WindowPlacement {
        session_id: session,
        window_id: state.window_id.clone(),
        window_index,
    };
    if let Some(existing) = members.get_mut(&state.pane_id) {
        if existing.state != state {
            return Err("linked rows disagreed about one pane");
        }
        if !existing.placements.insert(placement) {
            return Err("pane rows repeated one window placement");
        }
        return Ok(false);
    }
    let pane_id = state.pane_id.clone();
    members.insert(
        pane_id,
        PaneMember {
            state,
            placements: BTreeSet::from([placement]),
        },
    );
    Ok(true)
}

fn validate_link_rectangles(members: &BTreeMap<String, PaneMember>) -> Result<(), &'static str> {
    let mut windows = BTreeMap::<&str, &BTreeSet<WindowPlacement>>::new();
    for member in members.values() {
        match windows.get(member.state.window_id.as_str()) {
            Some(placements) if *placements != &member.placements => {
                return Err("linked pane rows formed an incomplete window placement");
            }
            Some(_) => {}
            None => {
                windows.insert(&member.state.window_id, &member.placements);
            }
        }
    }
    Ok(())
}

fn pane_snapshot(panes: &[libtmux::Pane]) -> Result<PaneSnapshot, &'static str> {
    let mut handles = BTreeMap::new();
    let mut members = BTreeMap::new();
    for pane in panes {
        let pane_id = pane.id().to_string();
        let state = PaneState {
            pane_id: pane_id.clone(),
            window_id: pane.window_id().to_string(),
            synchronized: pane.is_synchronized(),
            dead: pane.is_dead(),
            input_disabled: pane.is_input_disabled(),
            in_mode: pane.is_in_mode(),
            current_command: pane
                .current_command()
                .map(|command| command.as_bytes().to_vec()),
        };
        if merge_member(
            &mut members,
            state,
            pane.session_id().to_string(),
            pane.window_index(),
        )? {
            handles.insert(pane_id, pane.clone());
        }
    }
    validate_link_rectangles(&members)?;
    Ok(PaneSnapshot { handles, members })
}

fn validate_configured_members(
    configured: &[String],
    members: &BTreeMap<String, PaneMember>,
    attended: &BTreeSet<String>,
    generation: ServerGeneration,
    endpoint: &Path,
    reservation: Option<&PaneReservation>,
) -> Result<(), ErrorData> {
    for id in configured {
        let candidate = members
            .get(id)
            .ok_or_else(|| pane_snapshot_error("a selected pane disappeared"))?;
        if run_request::is_reserved(generation, endpoint, id, reservation) {
            return Err(active_run_error(id));
        }
        if attended.contains(id) {
            return Err(bad_input(format!(
                "pane {id} is visible to an attached terminal client; pane input is reserved for unattended panes"
            )));
        }
        if candidate.state.dead {
            return Err(bad_input(format!(
                "pane {id} is dead; pane input requires every configured recipient to be alive"
            )));
        }
        if candidate.state.input_disabled {
            return Err(bad_input(format!(
                "pane {id} has input disabled; pane input requires every configured recipient to accept input"
            )));
        }
        if candidate.state.in_mode {
            return Err(bad_input(format!(
                "pane {id} is in a tmux mode; wait for the attached client to leave the mode before sending input"
            )));
        }
    }
    Ok(())
}

fn configured_signature(
    configured: &[String],
    members: &BTreeMap<String, PaneMember>,
) -> Result<Vec<PaneMember>, ErrorData> {
    configured
        .iter()
        .map(|id| {
            members
                .get(id)
                .cloned()
                .ok_or_else(|| pane_snapshot_error("a selected pane disappeared"))
        })
        .collect()
}

impl TmuxTools {
    async fn pane_input_endpoint(&self) -> Result<PathBuf, ErrorData> {
        let result = self
            .server
            .cmd(
                Command::new("display-message")
                    .arg("-p")
                    .arg("#{socket_path}"),
            )
            .await
            .map_err(|error| tmux_error(&error))?;
        if let Some(error) = result.refusal_for("display-message") {
            return Err(tmux_error(&error));
        }
        let endpoint = result
            .stdout()
            .strip_suffix(b"\n")
            .filter(|path| !path.is_empty())
            .ok_or_else(|| {
                endpoint_error("tmux returned no resolved socket path for pane input")
            })?;
        let endpoint = PathBuf::from(OsString::from_vec(endpoint.to_vec()));
        if !crate::exec::route_path_is_terminal_safe(endpoint.as_os_str()) {
            return Err(endpoint_error(
                "the selected tmux socket contains an ASCII terminal-control byte",
            ));
        }
        Ok(endpoint)
    }

    pub(crate) async fn preflight_pane_input(
        &self,
        pane: &str,
        reach: PaneInputReach,
        missing: MissingSource,
    ) -> Result<PaneInputPlan, ErrorData> {
        self.preflight_pane_input_with_run(pane, reach, missing, None)
            .await
    }

    pub(crate) async fn preflight_reserved_pane_input(
        &self,
        pane: &str,
        reach: PaneInputReach,
        missing: MissingSource,
        reservation: &PaneReservation,
    ) -> Result<PaneInputPlan, ErrorData> {
        self.preflight_pane_input_with_run(pane, reach, missing, Some(reservation))
            .await
    }

    async fn preflight_pane_input_with_run(
        &self,
        pane: &str,
        reach: PaneInputReach,
        missing: MissingSource,
        reservation: Option<&PaneReservation>,
    ) -> Result<PaneInputPlan, ErrorData> {
        let generation = self
            .server
            .generation()
            .await
            .map_err(|error| tmux_error(&error))?;
        let panes = self
            .server
            .panes()
            .await
            .map_err(|error| tmux_error(&error))?;
        let snapshot = pane_snapshot(&panes).map_err(pane_snapshot_error)?;
        let source = snapshot
            .members
            .get(pane)
            .ok_or_else(|| missing_source_error(pane, missing))?;
        let target = snapshot
            .handles
            .get(pane)
            .cloned()
            .ok_or_else(|| missing_source_error(pane, missing))?;

        let configured =
            if matches!(reach, PaneInputReach::Synchronized) && source.state.synchronized {
                snapshot
                    .members
                    .values()
                    .filter(|candidate| {
                        candidate.state.window_id == source.state.window_id
                            && candidate.state.synchronized
                    })
                    .map(|candidate| candidate.state.pane_id.clone())
                    .collect::<Vec<_>>()
            } else {
                vec![source.state.pane_id.clone()]
            };

        let client_result = self
            .server
            .cmd(
                Command::new("list-clients")
                    .arg("-F")
                    .arg(CLIENT_ATTENTION_FORMAT),
            )
            .await
            .map_err(|error| tmux_error(&error))?;
        if let Some(error) = client_result.refusal_for("list-clients") {
            return Err(tmux_error(&error));
        }
        let attention = parse_client_attention(
            client_result.stdout(),
            &snapshot.members,
            &source.state.window_id,
        )
        .map_err(client_attention_error)?;
        let endpoint = self.pane_input_endpoint().await?;
        self.server
            .require_generation(generation)
            .await
            .map_err(|error| tmux_error(&error))?;
        let protected = self
            .caller_pane_for_snapshot(&endpoint, generation, &panes)?
            .map(str::to_owned);
        if let Some(own) = protected.as_deref()
            && configured.iter().any(|candidate| candidate == own)
        {
            return Err(Self::self_protection(format!(
                "refusing to send input to pane {own}: it matches this MCP server's inherited \
                 caller context, so input there may disrupt or end this conversation. Run the \
                 command in a terminal if that is what you meant."
            )));
        }

        validate_configured_members(
            &configured,
            &snapshot.members,
            &attention.attended,
            generation,
            &endpoint,
            reservation,
        )?;

        let signature = PaneInputSignature {
            source: source.clone(),
            configured: configured_signature(&configured, &snapshot.members)?,
            caller: self.caller.as_deref().cloned(),
            clients: attention.terminals,
        };

        Ok(PaneInputPlan {
            target,
            configured,
            endpoint,
            generation,
            signature,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        CLIENT_ATTENTION_FORMAT, MissingSource, PaneInputReach, PaneMember, PaneState,
        WindowPlacement, merge_member, parse_client_attention, validate_link_rectangles,
    };
    use crate::{CallerIdentity, TmuxTools};

    async fn caller_identity(
        server: &libtmux::Server,
        session: &str,
        pane: &str,
    ) -> CallerIdentity {
        let socket = server
            .cmd(
                libtmux::Command::new("display-message")
                    .arg("-p")
                    .arg("#{socket_path}"),
            )
            .await
            .expect("tmux reports its socket")
            .stdout_lossy()
            .trim()
            .to_owned();
        let generation = server.generation().await.expect("server generation");
        let session = session
            .strip_prefix('$')
            .expect("session id has its canonical prefix");
        CallerIdentity::from_values(
            Some(format!("{socket},{},{session}", generation.pid()).into()),
            Some(pane.into()),
        )
        .expect("caller context is present")
    }

    fn member(pane: &str, window: &str, placements: &[(&str, i32)]) -> PaneMember {
        PaneMember {
            state: PaneState {
                pane_id: pane.to_owned(),
                window_id: window.to_owned(),
                synchronized: false,
                dead: false,
                input_disabled: false,
                in_mode: false,
                current_command: Some(b"sh".to_vec()),
            },
            placements: placements
                .iter()
                .map(|(session, index)| WindowPlacement {
                    session_id: (*session).to_owned(),
                    window_id: window.to_owned(),
                    window_index: *index,
                })
                .collect(),
        }
    }

    fn members() -> BTreeMap<String, PaneMember> {
        [
            (
                "%0".to_owned(),
                member("%0", "@1", &[("$1", 0), ("$1", 7), ("$2", 7)]),
            ),
            (
                "%1".to_owned(),
                member("%1", "@1", &[("$1", 0), ("$1", 7), ("$2", 7)]),
            ),
            ("%9".to_owned(), member("%9", "@9", &[("$9", 4)])),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn client_attention_requests_full_placement() {
        assert_eq!(
            CLIENT_ATTENTION_FORMAT,
            "#{client_control_mode}|#{session_id}|#{window_id}|#{window_index}|#{pane_id}|#{window_zoomed_flag}"
        );
    }

    #[test]
    fn terminal_attention_follows_visibility() {
        assert_eq!(
            parse_client_attention(b"0|$2|@1|7|%1|1\n", &members(), "@1")
                .expect("zoomed row")
                .attended,
            ["%1"].into_iter().map(str::to_owned).collect()
        );
        assert_eq!(
            parse_client_attention(b"0|$1|@1|0|%1|0\n", &members(), "@1")
                .expect("visible window row")
                .attended,
            ["%0", "%1"].into_iter().map(str::to_owned).collect()
        );
        assert_eq!(
            parse_client_attention(b"0|$1|@1|7|%1|0\n", &members(), "@1")
                .expect("second placement in the same session")
                .attended,
            ["%0", "%1"].into_iter().map(str::to_owned).collect()
        );
    }

    #[test]
    fn control_and_other_window_clients_are_not_attended() {
        assert!(
            parse_client_attention(b"1|||||\n0|$9|@9|4|%9|0\n", &members(), "@1")
                .expect("valid unrelated rows")
                .attended
                .is_empty()
        );
    }

    #[test]
    fn terminal_rows_preserve_changes_with_equal_attention() {
        let first = parse_client_attention(b"0|$1|@1|0|%0|0\n", &members(), "@1")
            .expect("first terminal row");
        let second = parse_client_attention(b"0|$1|@1|0|%1|0\n", &members(), "@1")
            .expect("second terminal row");

        assert_eq!(first.attended, second.attended);
        assert_ne!(first.terminals, second.terminals);
    }

    #[tokio::test]
    async fn linked_placement_changes_the_transition_signature() {
        let guard = libtmux::test::TestServer::builder()
            .start()
            .await
            .expect("tmux starts");
        let server = guard.server();
        let session = server
            .new_session("input-placement-source")
            .await
            .expect("source session starts");
        let pane = session.panes().await.expect("source panes list").remove(0);
        let tools = TmuxTools::builder(server.clone()).caller(None).build();
        let initial = tools
            .preflight_pane_input(
                pane.id().as_ref(),
                PaneInputReach::TargetOnly,
                MissingSource::CallerInput,
            )
            .await
            .expect("initial input state is safe");
        let mut window = server
            .window_by_id(pane.window_id())
            .await
            .expect("source window lookup")
            .expect("source window exists");
        window
            .move_to(&session, 7)
            .await
            .expect("source window moves to another index");
        let fresh = tools
            .preflight_pane_input(
                pane.id().as_ref(),
                PaneInputReach::TargetOnly,
                MissingSource::CallerInput,
            )
            .await
            .expect("moved input state remains safe");

        assert!(
            !initial.same_authority(&fresh),
            "a window-index change changes the guarded snapshot"
        );
        guard.shutdown().await.expect("tmux fixture shuts down");
    }

    #[tokio::test]
    async fn one_session_can_link_the_same_window_at_multiple_indexes() {
        let guard = libtmux::test::TestServer::builder()
            .start()
            .await
            .expect("tmux starts");
        let server = guard.server();
        let session = server
            .new_session("input-repeated-link")
            .await
            .expect("session starts");
        let pane = session.panes().await.expect("source panes list").remove(0);
        server
            .window_by_id(pane.window_id())
            .await
            .expect("source window lookup")
            .expect("source window exists")
            .link_to(&session, Some(7))
            .await
            .expect("source window links twice in one session");

        TmuxTools::builder(server.clone())
            .caller(None)
            .build()
            .preflight_pane_input(
                pane.id().as_ref(),
                PaneInputReach::TargetOnly,
                MissingSource::CallerInput,
            )
            .await
            .expect("both window placements form one safe snapshot");

        guard.shutdown().await.expect("tmux fixture shuts down");
    }

    #[tokio::test]
    async fn caller_classification_is_part_of_the_transition_signature() {
        let guard = libtmux::test::TestServer::builder()
            .start()
            .await
            .expect("tmux starts");
        let server = guard.server();
        let pane = server
            .new_session("input-caller-signature")
            .await
            .expect("session starts")
            .panes()
            .await
            .expect("panes list")
            .remove(0);
        let plan = |caller| TmuxTools::builder(server.clone()).caller(caller).build();
        let detached = plan(None)
            .preflight_pane_input(
                pane.id().as_ref(),
                PaneInputReach::TargetOnly,
                MissingSource::CallerInput,
            )
            .await
            .expect("detached input is safe");
        let foreign = CallerIdentity::from_values(
            Some("/not-the-selected-tmux/socket,1,0".into()),
            Some("%9".into()),
        )
        .expect("foreign caller context is present");
        let foreign = plan(Some(foreign))
            .preflight_pane_input(
                pane.id().as_ref(),
                PaneInputReach::TargetOnly,
                MissingSource::CallerInput,
            )
            .await
            .expect("foreign caller is not selected");

        assert!(!detached.same_authority(&foreign));
        guard.shutdown().await.expect("tmux fixture shuts down");
    }

    #[tokio::test]
    async fn caller_resolves_through_a_linked_session_placement() {
        let guard = libtmux::test::TestServer::builder()
            .start()
            .await
            .expect("tmux starts");
        let server = guard.server();
        let source = server
            .new_session("input-caller-source")
            .await
            .expect("source session starts");
        let pane = source.panes().await.expect("source panes list").remove(0);
        let linked = server
            .new_session("input-caller-linked")
            .await
            .expect("linked session starts");
        server
            .window_by_id(pane.window_id())
            .await
            .expect("source window lookup")
            .expect("source window exists")
            .link_to(&linked, None)
            .await
            .expect("source window links into another session");
        let caller = caller_identity(server, linked.id().as_ref(), pane.id().as_ref()).await;

        let error = TmuxTools::builder(server.clone())
            .caller(Some(caller))
            .build()
            .preflight_pane_input(
                pane.id().as_ref(),
                PaneInputReach::TargetOnly,
                MissingSource::CallerInput,
            )
            .await
            .err()
            .expect("the linked caller placement remains protected");

        assert_eq!(
            error.data.expect("typed refusal")["kind"],
            "self_protection"
        );
        guard.shutdown().await.expect("tmux fixture shuts down");
    }

    #[test]
    fn terminal_client_placement_must_match_the_full_snapshot() {
        for stdout in [
            b"0|$1|@1|0|%8|0\n".as_slice(),
            b"0|$8|@1|0|%1|0\n",
            b"0|$1|@8|0|%1|0\n",
            b"0|$1|@1|8|%1|0\n",
        ] {
            assert!(
                parse_client_attention(stdout, &members(), "@1").is_err(),
                "inconsistent placement was accepted: {stdout:?}"
            );
        }
    }

    #[test]
    fn malformed_client_attention_fails_closed() {
        for stdout in [
            b"0|$1|@1|0|%0|0".as_slice(),
            b"|$1|@1|0|%0|0\n",
            b"on|$1|@1|0|%0|0\n",
            b"0||@1|0|%0|0\n",
            b"0|$01|@1|0|%0|0\n",
            b"0|$1|@01|0|%0|0\n",
            b"0|$1|@1|00|%0|0\n",
            b"0|$1|@1|+0|%0|0\n",
            b"0|$1|@1|-0|%0|0\n",
            b"0|$1|@1|2147483648|%0|0\n",
            b"0|$1|@1|0|%01|0\n",
            b"0|$4294967296|@1|0|%0|0\n",
            b"0|$1|@4294967296|0|%0|0\n",
            b"0|$1|@1|0|%4294967296|0\n",
            b"0|$1|@1|0|%0|\n",
            b"0|$1|@1|0|%0|on\n",
            b"0|$1|@1|0|%0\n",
            b"0|$1|@1|0|%0|0|tail\n",
            b"0|$1|@1|0|%0|0\n\n",
            b"0|$1|@1|0|\xff|0\n",
        ] {
            assert!(
                parse_client_attention(stdout, &members(), "@1").is_err(),
                "malformed row was accepted: {stdout:?}"
            );
        }
    }

    #[test]
    fn linked_rows_must_agree_on_pane_state() {
        let base = member("%0", "@1", &[("$1", 0)]).state;
        let mut rows = BTreeMap::new();
        assert!(merge_member(&mut rows, base.clone(), "$1".to_owned(), 0).expect("first row"));
        assert!(
            !merge_member(&mut rows, base.clone(), "$1".to_owned(), 7)
                .expect("second placement in the same session")
        );
        assert!(!merge_member(&mut rows, base.clone(), "$2".to_owned(), 7).expect("linked row"));
        assert_eq!(
            rows["%0"].placements,
            BTreeSet::from([
                WindowPlacement {
                    session_id: "$1".to_owned(),
                    window_id: "@1".to_owned(),
                    window_index: 0,
                },
                WindowPlacement {
                    session_id: "$1".to_owned(),
                    window_id: "@1".to_owned(),
                    window_index: 7,
                },
                WindowPlacement {
                    session_id: "$2".to_owned(),
                    window_id: "@1".to_owned(),
                    window_index: 7,
                },
            ])
        );

        assert!(merge_member(&mut rows, base.clone(), "$2".to_owned(), 7).is_err());

        for changed in [
            PaneState {
                window_id: "@2".to_owned(),
                ..base.clone()
            },
            PaneState {
                synchronized: true,
                ..base.clone()
            },
            PaneState {
                dead: true,
                ..base.clone()
            },
            PaneState {
                input_disabled: true,
                ..base.clone()
            },
            PaneState {
                in_mode: true,
                ..base.clone()
            },
            PaneState {
                current_command: Some(b"zsh".to_vec()),
                ..base.clone()
            },
        ] {
            assert!(merge_member(&mut rows, changed, "$3".to_owned(), 9).is_err());
        }
    }

    #[test]
    fn linked_window_placements_form_complete_rectangles() {
        let complete = BTreeMap::from([
            (
                "%0".to_owned(),
                member("%0", "@1", &[("$1", 0), ("$1", 7), ("$2", 7)]),
            ),
            (
                "%1".to_owned(),
                member("%1", "@1", &[("$1", 0), ("$1", 7), ("$2", 7)]),
            ),
        ]);
        assert!(validate_link_rectangles(&complete).is_ok());

        let incomplete = BTreeMap::from([
            (
                "%0".to_owned(),
                member("%0", "@1", &[("$1", 0), ("$1", 7), ("$2", 7)]),
            ),
            ("%1".to_owned(), member("%1", "@1", &[("$1", 0), ("$2", 7)])),
        ]);
        assert!(validate_link_rectangles(&incomplete).is_err());
    }
}
