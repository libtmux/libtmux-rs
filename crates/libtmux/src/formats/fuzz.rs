//! The format-row codec, reachable from `fuzz/` without becoming public.

use super::plan::TransportDialect;
use super::row::{FIELD_SEPARATOR, encode_like_tmux};
use super::{FormatPlan, ListProfile};
use crate::snapshot::{
    hydrate_client_infos_from_stdout, hydrate_pane_projections_from_stdout,
    hydrate_session_infos_from_stdout, hydrate_window_projections_from_stdout,
    pane_projection_plan, window_projection_plan,
};
use crate::{ServerIdentity, TmuxVersion};

/// `format_quote_shell`'s set from 3.2a through 3.7.
const Q_ESCAPED_BEFORE_3_8: &[u8] = b"|&;<>()$`\\\"'*?[# =%";
/// `format_quote_shell`'s set from 3.8-rc, which added `{}`, newline and tab.
const Q_ESCAPED_FROM_3_8: &[u8] = b"|&;<>(){}$`\\\"'*?[# =%\n\t";

/// The releases a selector picks, each with the set it quotes with. 3.5 is
/// the `vis` dialect; `next-3.9` stands for 3.8-rc and later.
const RELEASES: [(&[u8], &[u8]); 4] = [
    (b"tmux 3.2a\n", Q_ESCAPED_BEFORE_3_8),
    (b"tmux 3.5\n", Q_ESCAPED_BEFORE_3_8),
    (b"tmux 3.7d\n", Q_ESCAPED_BEFORE_3_8),
    (b"tmux next-3.9\n", Q_ESCAPED_FROM_3_8),
];

/// Decode arbitrary listing output, then check that decoding inverts tmux.
///
/// The first byte picks a listing and a tmux release. The rest is decoded as
/// that listing's stdout, and then, split at NUL, read as field values that
/// are written the way that release prints them and decoded again.
///
/// # Panics
///
/// When a value tmux could print does not decode to itself.
#[doc(hidden)]
pub fn __fuzz_format_rows(data: &[u8]) {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let (release, escaped) = RELEASES[usize::from(selector & 0b11)];
    let Ok(version) = TmuxVersion::parse_output(release) else {
        return;
    };
    let identity = ServerIdentity::from_socket_path("/fuzz".into());
    let plan = match (selector >> 2) & 0b11 {
        0 => {
            let plan = FormatPlan::for_profile(ListProfile::Sessions, &version);
            let _ = hydrate_session_infos_from_stdout(&plan, rest);
            plan
        }
        1 => {
            let plan = FormatPlan::for_profile(ListProfile::Clients, &version);
            let _ = hydrate_client_infos_from_stdout(&plan, rest);
            plan
        }
        2 => {
            let Ok(plan) = window_projection_plan(&version) else {
                return;
            };
            let _ = hydrate_window_projections_from_stdout(&identity, &plan, rest);
            plan
        }
        _ => {
            let Ok(plan) = pane_projection_plan(&version) else {
                return;
            };
            let _ = hydrate_pane_projections_from_stdout(&identity, &plan, rest);
            plan
        }
    };
    round_trip(&plan, rest, escaped);
}

/// Write NUL-separated `values` as rows of `plan`, and decode them back.
fn round_trip(plan: &FormatPlan, values: &[u8], escaped: &[u8]) {
    let fields = plan.descriptors.len();
    let mut expected: Vec<&[u8]> = values.split(|&byte| byte == 0).collect();
    expected.resize(expected.len().next_multiple_of(fields), b"");

    let mut wire = Vec::new();
    for (field, value) in expected.iter().enumerate() {
        encode_like_tmux(value, escaped, plan.dialect, &mut wire);
        wire.push(FIELD_SEPARATOR);
        if field % fields == fields - 1 {
            wire.push(b'\n');
        }
    }

    let rows = plan.parse_rows(&wire);
    let dialect = plan.dialect == TransportDialect::Vis;
    assert!(
        rows.is_ok(),
        "{:?} decoding {wire:?} (vis: {dialect})",
        rows.as_ref().err()
    );
    let decoded: Vec<Vec<u8>> = rows
        .iter()
        .flatten()
        .flat_map(|row| row.slots().map(|slot| slot.as_bytes().to_vec()))
        .collect();
    assert_eq!(decoded, expected, "{wire:?} (vis: {dialect})");
}
