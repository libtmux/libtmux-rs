//! What this server itself typed into a pane, so `wait_for_text` can tell
//! the pane's answer from its own question.
//!
//! A pane echoes what is typed into it. `send_keys` with `text: "echo
//! MARKER", enter: true` types a line containing the very pattern a caller
//! is about to wait for, and the line scrolls into the pane's completed
//! output as soon as Enter is processed -- before the command it names has
//! run. [`crate::exec::wait_for_text`]'s row-position split already keeps a
//! pattern still on the row being typed into from matching; this covers the
//! row after it is submitted, which position alone cannot tell from real
//! output.
//!
//! Tracking is best-effort and, on any doubt, fails toward showing text
//! rather than hiding it. A key this cannot represent as literal text --
//! `Left`, `Home`, a function key -- clears the pane's tracked line instead
//! of masking stale text a subsequent edit may have moved past: the
//! consequence is that an echo may then read as a match, never that real
//! output goes missing because it happened to repeat a phrase this recorded.

use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use libtmux::ServerGeneration;

use crate::run_request::{EndpointIdentity, endpoint_identity};

/// How long a submitted line's echo is still worth discounting.
///
/// Long enough to cover the gap between submitting a command and a
/// `wait_for_text` call that started before or shortly after it; short
/// enough that a pane reused for something else is not still suppressing
/// text that only looks like an old echo.
const RECENT_TTL: Duration = Duration::from_secs(10);

/// Submitted lines kept per pane, oldest dropped first.
const RECENT_PER_PANE: usize = 4;

/// Panes tracked at once, across every server this process has selected.
///
/// Evicting the least recently touched pane when this is exceeded is what
/// keeps a killed pane's entry from living for the life of the process when
/// nothing ever ages it out on its own.
const MAX_TRACKED_PANES: usize = 256;

/// One pane, identified so a reused pane id after a server restart never
/// inherits another server's record.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct EchoKey {
    generation: ServerGeneration,
    endpoint: EndpointIdentity,
    pane: String,
}

impl EchoKey {
    /// Build a key from a pane input dispatch's already-resolved endpoint and
    /// server generation, or from a fresh read for a caller that has neither
    /// in hand.
    ///
    /// `None` when the endpoint cannot be identified; the caller is meant to
    /// treat that as "track nothing for this call" rather than propagate a
    /// failure through a path this is not load-bearing for.
    pub(crate) fn new(generation: ServerGeneration, endpoint: &Path, pane: &str) -> Option<Self> {
        Some(Self {
            generation,
            endpoint: endpoint_identity(endpoint).ok()?,
            pane: pane.to_owned(),
        })
    }
}

#[derive(Default)]
struct PaneRecord {
    /// This pane's current line: typed but neither submitted nor cleared.
    ///
    /// Only meaningful when `in_flight` is `0`: a dispatch computes what
    /// this should become before tmux has seen any of it, and this is not
    /// updated until that dispatch is confirmed, so a concurrent reader
    /// never sees a line reported empty while the real terminal is still
    /// mid-typing it.
    pending: String,
    /// How many dispatches to this pane have computed a new `pending` but
    /// not yet confirmed it against tmux.
    in_flight: u32,
    /// Lines recently submitted on this pane, oldest first, each with when.
    recent: VecDeque<(String, Instant)>,
    /// Last time this pane was touched, for bounding the outer table.
    touched: Option<Instant>,
}

impl PaneRecord {
    fn push_recent(&mut self, line: String, now: Instant) {
        if let Some((last, at)) = self.recent.back_mut()
            && *last == line
        {
            // The same line twice is one record, freshened: a shell that
            // redraws a submitted line on completion must not push an
            // earlier, different echo out of a short, bounded list.
            *at = now;
            return;
        }
        self.recent.push_back((line, now));
        while self.recent.len() > RECENT_PER_PANE {
            self.recent.pop_front();
        }
    }

    fn prune(&mut self, now: Instant) {
        while self
            .recent
            .front()
            .is_some_and(|(_, at)| now.duration_since(*at) > RECENT_TTL)
        {
            self.recent.pop_front();
        }
    }

    /// Whether this pane's current line can safely be treated as something
    /// other than this server's own mid-typing.
    fn has_pending(&self) -> bool {
        self.in_flight > 0 || !self.pending.is_empty()
    }

    fn is_empty(&self, now: Instant) -> bool {
        self.in_flight == 0
            && self.pending.is_empty()
            && self.recent.is_empty()
            && self
                .touched
                .is_none_or(|touched| now.duration_since(touched) > RECENT_TTL)
    }
}

/// What one `send_keys` dispatch does to a pane's still-being-typed line.
struct DispatchOutcome {
    /// The line this dispatch submitted, when one of its keys did.
    submitted: Option<String>,
}

/// tmux key names that submit a line, as Enter does.
const SUBMIT_KEYS: [&str; 3] = ["Enter", "C-m", "KPEnter"];
/// tmux key names that discard the line without submitting it.
const KILL_LINE_KEYS: [&str; 2] = ["C-u", "C-c"];
/// tmux key names that remove the character before the cursor.
const ERASE_KEYS: [&str; 2] = ["BSpace", "C-h"];

/// One printable character, when `key` names exactly one rather than naming
/// a key with no character of its own.
fn typed_char(key: &str) -> Option<char> {
    if key == "Space" {
        return Some(' ');
    }
    let mut chars = key.chars();
    let only = chars.next()?;
    if chars.next().is_some() || only.is_control() {
        return None;
    }
    Some(only)
}

/// Apply one dispatch's `text` (typed literally, verbatim, first) and `keys`
/// (interpreted, in order) to `pending`.
///
/// An unmodelable key clears `pending` -- it stops discounting this pane's
/// line rather than keep text an edit it cannot represent may have moved
/// past -- and processing continues after it, so a recognized key later in
/// the same dispatch still starts a fresh, accurate line.
fn apply_dispatch(pending: &mut String, text: Option<&str>, keys: &[String]) -> DispatchOutcome {
    if let Some(text) = text.filter(|text| !text.is_empty()) {
        pending.push_str(text);
    }
    let mut submitted = None;
    for key in keys {
        let key = key.as_str();
        if SUBMIT_KEYS.contains(&key) {
            let line = std::mem::take(pending);
            if !line.is_empty() {
                submitted = Some(line);
            }
        } else if KILL_LINE_KEYS.contains(&key) {
            pending.clear();
        } else if ERASE_KEYS.contains(&key) {
            pending.pop();
        } else if key == "DC" {
            // A pending line only ever grows at the cursor, so the cursor is
            // always at its end; forward-delete there has nothing to remove.
        } else if let Some(character) = typed_char(key) {
            pending.push(character);
        } else {
            pending.clear();
        }
    }
    DispatchOutcome { submitted }
}

/// What [`PaneEchoes::apply`] computed for one dispatch, to be confirmed with
/// [`PaneEchoes::commit`] or given up on with [`PaneEchoes::abandon`].
///
/// Dropping this without calling either leaks that dispatch's share of
/// `in_flight`, which would leave the pane's line permanently excluded from
/// [`PaneEchoes::has_pending`]'s relaxation; every path in `send_keys_one`
/// that can end a dispatch must reach one of the two.
pub(crate) struct EchoUpdate {
    /// The pane's line as it will read once this dispatch is confirmed.
    pending: Vec<(EchoKey, String)>,
}

/// What this process has typed into panes it has touched, kept per tmux
/// server generation and pane id.
#[derive(Default)]
pub(crate) struct PaneEchoes {
    inner: Mutex<HashMap<EchoKey, PaneRecord>>,
}

impl PaneEchoes {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Compute what every pane a dispatch reaches (its configured, possibly
    /// synchronized, cohort) will owe to `text` and `keys`, before that
    /// dispatch reaches tmux.
    ///
    /// A line this dispatch submits is recorded as a fresh echo immediately,
    /// not deferred to [`Self::commit`]: the terminal's echo can reach a
    /// waiting `wait_for_text` client before this call even returns, so
    /// masking it has to be in place that early. The pane's current line
    /// itself is not published yet, because a real one is genuinely still
    /// being typed on the wire until tmux confirms it, and reporting it
    /// empty in that window would let `wait_for_text` treat mid-typing text
    /// as the pane's own settled state.
    pub(crate) fn apply(
        &self,
        generation: ServerGeneration,
        endpoint: &Path,
        panes: &[String],
        text: Option<&str>,
        keys: &[String],
    ) -> EchoUpdate {
        let Ok(endpoint) = endpoint_identity(endpoint) else {
            return EchoUpdate {
                pending: Vec::new(),
            };
        };
        let now = Instant::now();
        let mut table = self.hold();
        let mut pending = Vec::with_capacity(panes.len());
        for pane in panes {
            let key = EchoKey {
                generation,
                endpoint,
                pane: pane.clone(),
            };
            let record = table.entry(key.clone()).or_default();
            record.in_flight += 1;
            record.touched = Some(now);
            let mut scratch = record.pending.clone();
            let outcome = apply_dispatch(&mut scratch, text, keys);
            if let Some(line) = outcome.submitted {
                record.push_recent(line, now);
            }
            pending.push((key, scratch));
        }
        evict_stale(&mut table, now);
        EchoUpdate { pending }
    }

    /// Publish an [`Self::apply`] call's computed line once tmux has
    /// confirmed the dispatch that produced it.
    pub(crate) fn commit(&self, update: EchoUpdate) {
        let now = Instant::now();
        let mut table = self.hold();
        for (key, computed) in update.pending {
            if let Some(record) = table.get_mut(&key) {
                record.pending = computed;
                record.in_flight = record.in_flight.saturating_sub(1);
                record.touched = Some(now);
            }
        }
        evict_stale(&mut table, now);
    }

    /// Give up on an [`Self::apply`] call whose dispatch never reached tmux.
    ///
    /// The computed line is discarded, not published: the pane's line is
    /// exactly what it was before this dispatch. The echo already pushed to
    /// `recent`, if any, is kept regardless -- masking a line that never
    /// appears on the pane removes nothing real.
    pub(crate) fn abandon(&self, update: EchoUpdate) {
        let mut table = self.hold();
        for (key, _) in update.pending {
            if let Some(record) = table.get_mut(&key) {
                record.in_flight = record.in_flight.saturating_sub(1);
            }
        }
    }

    /// Whether `pane`'s current line can safely be treated as something
    /// other than this server's own mid-typing.
    pub(crate) fn has_pending(&self, key: &EchoKey) -> bool {
        self.hold().get(key).is_some_and(PaneRecord::has_pending)
    }

    /// Lines this pane has recently submitted, still young enough to
    /// discount, as the bytes a captured screen would show them in.
    pub(crate) fn snapshot(&self, key: &EchoKey) -> Vec<Vec<u8>> {
        let now = Instant::now();
        let mut table = self.hold();
        let Some(record) = table.get_mut(key) else {
            return Vec::new();
        };
        record.prune(now);
        record
            .recent
            .iter()
            .map(|(line, _)| line.clone().into_bytes())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn tracked_panes(&self) -> usize {
        self.hold().len()
    }

    fn hold(&self) -> std::sync::MutexGuard<'_, HashMap<EchoKey, PaneRecord>> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Drop panes that carry nothing worth keeping, then the least recently
/// touched over the table's cap -- so a killed pane's entry does not live
/// for the life of the process even when nothing else ages it out.
fn evict_stale(table: &mut HashMap<EchoKey, PaneRecord>, now: Instant) {
    table.retain(|_, record| {
        record.prune(now);
        !record.is_empty(now)
    });
    while table.len() > MAX_TRACKED_PANES {
        let Some(stale) = table
            .iter()
            .min_by_key(|(_, record)| record.touched)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        table.remove(&stale);
    }
}

/// Whether `byte` can be part of the word a masked echo must not tear into.
///
/// A high-bit byte (part of a multi-byte UTF-8 sequence) counts as a word
/// byte too, conservatively: treating it as a boundary risks masking half of
/// a character next to one this recorded, and this crate's patterns and
/// recorded echoes are ASCII in every case that matters here.
const fn is_word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80
}

/// Remove every whole-word-bounded occurrence of `needle` from `haystack`.
///
/// Only a word boundary, not "ends its line": a shell draws its own things
/// straight after what was typed (a right-hand prompt, a redraw on submit),
/// so insisting on the end of a row let such echoes through. The cost is
/// that real output repeating the typed text as a word of its own loses that
/// word along with it; a wait for exactly that text is not one a screen can
/// answer, whichever side wrote it.
///
/// A submitted line that wrapped across terminal rows when it was typed is
/// not found here and is a known gap: `above` inserts a real newline at
/// every captured row, wrapped or not, because this crate does not join
/// wrapped lines (`-J` breaks the cursor arithmetic `wait_for_text` relies
/// on), so a wrapped echo's recorded text never matches the broken-up rows.
fn without_echo(haystack: &[u8], needle: &[u8]) -> Vec<u8> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return haystack.to_vec();
    }
    let mut masked = vec![false; haystack.len()];
    let mut from = 0;
    while from + needle.len() <= haystack.len() {
        let Some(offset) = haystack[from..]
            .windows(needle.len())
            .position(|window| window == needle)
        else {
            break;
        };
        let at = from + offset;
        let end = at + needle.len();
        let opens = at == 0 || !is_word_byte(haystack[at - 1]);
        let closes = end == haystack.len() || !is_word_byte(haystack[end]);
        if opens && closes {
            for slot in &mut masked[at..end] {
                *slot = true;
            }
            from = end;
        } else {
            from = at + 1;
        }
    }
    haystack
        .iter()
        .zip(masked)
        .filter_map(|(&byte, hit)| (!hit).then_some(byte))
        .collect()
}

/// Remove every recorded echo from `haystack`, in order.
pub(crate) fn mask(haystack: &[u8], echoes: &[Vec<u8>]) -> Vec<u8> {
    let mut text = haystack.to_vec();
    for echo in echoes {
        text = without_echo(&text, echo);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_submitted_line_is_masked_whole_word_only() {
        let haystack = b"echo MARKER\nMARKER\nprefixMARKER\n";
        let masked = mask(haystack, &[b"echo MARKER".to_vec()]);
        assert_eq!(masked, b"\nMARKER\nprefixMARKER\n");
    }

    #[test]
    fn masking_never_tears_a_longer_word() {
        let masked = without_echo(b"prefixMARKERsuffix", b"MARKER");
        assert_eq!(masked, b"prefixMARKERsuffix");
    }

    #[test]
    fn repeated_output_still_matches_after_masking_the_echo() {
        let haystack = b"sleep 1; echo MARKER\nMARKER\n";
        let masked = mask(haystack, &[b"sleep 1; echo MARKER".to_owned().to_vec()]);
        assert_eq!(masked, b"\nMARKER\n");
    }

    #[test]
    fn backspaces_reach_an_earlier_calls_pending_text() {
        let mut pending = String::new();
        apply_dispatch(&mut pending, Some("xMARKER"), &[]);
        assert_eq!(pending, "xMARKER");
        let backspaces = vec!["BSpace".to_owned(); 7];
        apply_dispatch(&mut pending, None, &backspaces);
        assert_eq!(pending, "");
        let outcome = apply_dispatch(&mut pending, Some("echo MARKER"), &["Enter".to_owned()]);
        assert_eq!(outcome.submitted.as_deref(), Some("echo MARKER"));
        assert_eq!(pending, "");
    }

    #[test]
    fn an_unmodelable_key_clears_pending_instead_of_keeping_it_stale() {
        let mut pending = "MARKER".to_owned();
        let outcome = apply_dispatch(&mut pending, None, &["Left".to_owned()]);
        assert!(outcome.submitted.is_none());
        assert_eq!(
            pending, "",
            "an unrecognized key stops discounting the line"
        );
    }

    #[test]
    fn kill_line_keys_discard_without_recording_an_echo() {
        let mut pending = "doomed".to_owned();
        let outcome = apply_dispatch(&mut pending, None, &["C-u".to_owned()]);
        assert!(outcome.submitted.is_none());
        assert_eq!(pending, "");
    }

    /// A pane's record is bounded in lifetime (TTL) and the table in size
    /// (a cap), so a killed pane's entry does not live for the life of the
    /// process even when nothing else ages it out.
    ///
    /// `ServerGeneration` has no public constructor -- correctly, since a
    /// fabricated one could collide with a real server's -- so this reads
    /// one real value from a fixture rather than faking it; `apply` itself
    /// makes no tmux round trip, so this stays well inside the inner-loop
    /// budget despite exercising the table at its cap.
    #[tokio::test]
    async fn a_killed_panes_record_does_not_outlive_the_table_cap() {
        let guard = libtmux::test::TestServer::builder()
            .start()
            .await
            .expect("tmux starts");
        let generation = guard
            .server()
            .generation()
            .await
            .expect("server generation");
        let endpoint = guard.server().socket_path().to_path_buf();
        let echoes = PaneEchoes::new();

        for index in 0..MAX_TRACKED_PANES + 8 {
            let pane = format!("%{index}");
            let _ = echoes.apply(
                generation,
                &endpoint,
                std::slice::from_ref(&pane),
                Some("x"),
                &[],
            );
        }

        assert!(
            echoes.tracked_panes() <= MAX_TRACKED_PANES,
            "the table stays bounded at {} panes",
            echoes.tracked_panes()
        );

        guard.shutdown().await.expect("tmux fixture shuts down");
    }
}
