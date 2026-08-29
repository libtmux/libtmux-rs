//! The tmux hierarchy as a browsable URI space.
//!
//! Tools are what an agent calls; resources are what a person attaches. A
//! client shows these in a picker, so "put that pane in the conversation" is a
//! choice someone makes directly rather than a call an agent has to be talked
//! into. Nothing here is reachable only this way -- every value also has a
//! tool -- but a listing an agent must be asked for is not the same as one a
//! human can point at.
//!
//! The URIs mirror the hierarchy, so a reader who knows tmux can guess them:
//!
//! ```text
//! tmux://server                                  selected server and inherited caller context
//! tmux://sessions                                every session
//! tmux://sessions/{name}                         one session
//! tmux://sessions/{name}/windows                 its windows
//! tmux://sessions/{name}/windows/{index}         one window
//! tmux://windows                                 every window, across sessions
//! tmux://panes                                   every pane, across sessions
//! tmux://panes/{id}                              one pane
//! tmux://panes/{id}/content                      what that pane is showing
//! ```
//!
//! The Python server takes a `{?socket_name}` query on each of these, because
//! it picks a server per call. This one is bound to a socket when it starts,
//! so the query would be a way to reach a tmux the operator did not choose.
//! It is left out.

use rmcp::ErrorData;
use rmcp::model::{
    ListResourceTemplatesResult, ListResourcesResult, ReadResourceResult, Resource,
    ResourceContents, ResourceTemplate,
};

/// `application/json` for structured values, matching what the tools answer.
const JSON: &str = "application/json";

/// `text/plain` for pane contents, which are what a terminal drew.
const TEXT: &str = "text/plain";

/// The resources that exist without naming anything.
///
/// A client lists these; the templated ones below are advertised separately,
/// because a listing that enumerated every pane would go stale the moment a
/// pane closed.
#[must_use]
pub fn listed() -> ListResourcesResult {
    let entries = [
        (
            "tmux://server",
            "tmux server",
            "The selected tmux server's socket and session count, plus the pane id \
             inherited at launch when there was one.",
            JSON,
        ),
        (
            "tmux://sessions",
            "sessions",
            "Every session on this server.",
            JSON,
        ),
        (
            "tmux://windows",
            "windows",
            "Every window on this server, across all sessions.",
            JSON,
        ),
        (
            "tmux://panes",
            "panes",
            "Every pane on this server, across all sessions.",
            JSON,
        ),
    ];

    ListResourcesResult::with_all_items(
        entries
            .into_iter()
            .map(|(uri, name, description, mime)| {
                Resource::new(uri, name)
                    .with_description(description)
                    .with_mime_type(mime)
            })
            .collect(),
    )
}

/// The resources that need a name or an id to point at something.
#[must_use]
pub fn templates() -> ListResourceTemplatesResult {
    let entries = [
        (
            "tmux://sessions/{session_name}",
            "session",
            "One session, by name.",
            JSON,
        ),
        (
            "tmux://sessions/{session_name}/windows",
            "session windows",
            "The windows in one session, by session name.",
            JSON,
        ),
        (
            "tmux://sessions/{session_name}/windows/{window_index}",
            "window",
            "One window, by session name and index.",
            JSON,
        ),
        ("tmux://panes/{pane_id}", "pane", "One pane, by id.", JSON),
        (
            "tmux://panes/{pane_id}/content",
            "pane content",
            "What a pane is showing, as text. This is the visible screen; use \
             the capture_pane tool for scrollback.",
            TEXT,
        ),
    ];

    ListResourceTemplatesResult::with_all_items(
        entries
            .into_iter()
            .map(|(uri, name, description, mime)| {
                ResourceTemplate::new(uri, name)
                    .with_description(description)
                    .with_mime_type(mime)
            })
            .collect(),
    )
}

/// What a URI names, once it has been taken apart.
///
/// Parsing is separated from answering so the shapes can be tested without a
/// tmux server, and so an unknown URI is one `_` arm rather than a fallthrough
/// buried in the middle of a lookup.
#[derive(Debug, Eq, PartialEq)]
pub enum Target {
    /// `tmux://server`
    Server,
    /// `tmux://sessions`
    Sessions,
    /// `tmux://windows`
    Windows,
    /// `tmux://panes`
    Panes,
    /// `tmux://sessions/{name}`
    Session(String),
    /// `tmux://sessions/{name}/windows`
    SessionWindows(String),
    /// `tmux://sessions/{name}/windows/{index}`
    Window(String, String),
    /// `tmux://panes/{id}`
    Pane(String),
    /// `tmux://panes/{id}/content`
    PaneContent(String),
}

impl Target {
    /// Take a URI apart, or report that it names nothing here.
    ///
    /// Session names are percent-decoded, because a session may be called
    /// `my project` and a client filling in a template will encode the space.
    /// Pane ids are not: tmux spells them `%1`, so the sigil that starts every
    /// one of them is also the character that starts an escape. Decoding would
    /// make `%25` ambiguous between pane 25 and an encoded `%`, and pane 25 is
    /// the reading that can actually occur.
    #[must_use]
    pub fn parse(uri: &str) -> Option<Self> {
        let rest = uri.strip_prefix("tmux://")?;
        let raw: Vec<&str> = rest.split('/').collect();

        match raw.as_slice() {
            ["server"] => Some(Self::Server),
            ["sessions"] => Some(Self::Sessions),
            ["windows"] => Some(Self::Windows),
            ["panes"] => Some(Self::Panes),
            ["sessions", name] if !name.is_empty() => Some(Self::Session(session_name(name)?)),
            ["sessions", name, "windows"] if !name.is_empty() => {
                Some(Self::SessionWindows(session_name(name)?))
            }
            ["sessions", name, "windows", index] if !name.is_empty() && !index.is_empty() => {
                Some(Self::Window(session_name(name)?, (*index).to_owned()))
            }
            ["panes", id] if id.starts_with('%') => Some(Self::Pane((*id).to_owned())),
            ["panes", id, "content"] if id.starts_with('%') => {
                Some(Self::PaneContent((*id).to_owned()))
            }
            _ => None,
        }
    }
}

/// Decode a session name, refusing anything that is not valid UTF-8.
///
/// Written out rather than pulled in: this is the only place the crate needs
/// it, and a dependency for one loop is a dependency to keep updated forever.
fn session_name(part: &str) -> Option<String> {
    if !part.contains('%') {
        return Some(part.to_owned());
    }
    let bytes = part.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Answer with one JSON value.
///
/// # Errors
///
/// Returns an error when the value cannot be serialised, which would mean a
/// view type that is no longer serialisable.
pub fn json(uri: &str, value: &impl serde::Serialize) -> Result<ReadResourceResult, ErrorData> {
    let body = serde_json::to_string_pretty(value)
        .map_err(|error| ErrorData::internal_error(error.to_string(), None))?;
    Ok(ReadResourceResult::new(vec![
        ResourceContents::text(body, uri).with_mime_type(JSON),
    ]))
}

/// Answer with plain text, which is what a pane holds.
#[must_use]
pub fn text(uri: &str, body: String) -> ReadResourceResult {
    ReadResourceResult::new(vec![ResourceContents::text(body, uri).with_mime_type(TEXT)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_resource_parses_back_to_a_target() {
        // A listing that advertises a URI `read_resource` cannot take apart is
        // a dead entry in every client's picker.
        for resource in listed().resources {
            let uri = resource.uri.as_str();
            assert!(
                Target::parse(uri).is_some(),
                "{uri} is advertised but does not parse",
            );
        }
    }

    #[test]
    fn every_template_parses_once_its_placeholders_are_filled() {
        let filled = [
            ("tmux://sessions/{session_name}", "tmux://sessions/work"),
            (
                "tmux://sessions/{session_name}/windows",
                "tmux://sessions/work/windows",
            ),
            (
                "tmux://sessions/{session_name}/windows/{window_index}",
                "tmux://sessions/work/windows/2",
            ),
            ("tmux://panes/{pane_id}", "tmux://panes/%1"),
            ("tmux://panes/{pane_id}/content", "tmux://panes/%1/content"),
        ];
        let advertised: Vec<String> = templates()
            .resource_templates
            .iter()
            .map(|template| template.uri_template.clone())
            .collect();
        assert_eq!(
            advertised.len(),
            filled.len(),
            "every template needs a worked example here: {advertised:?}",
        );
        for (template, example) in filled {
            assert!(
                advertised.iter().any(|a| a == template),
                "{template} is no longer advertised",
            );
            assert!(
                Target::parse(example).is_some(),
                "{example} does not parse, so {template} cannot be read",
            );
        }
    }

    #[test]
    fn a_pane_id_survives_being_a_uri() {
        // `%1` is a pane id and `%` starts an escape. A decoder applied here
        // would either reject it -- `%1` is not two hex digits -- or, worse,
        // read `%25` as `%` and hand back a pane nobody named.
        assert_eq!(
            Target::parse("tmux://panes/%1"),
            Some(Target::Pane("%1".to_owned())),
        );
        assert_eq!(
            Target::parse("tmux://panes/%25"),
            Some(Target::Pane("%25".to_owned())),
            "pane 25 stays pane 25 rather than decoding to a bare percent",
        );
        assert_eq!(
            Target::parse("tmux://panes/%1/content"),
            Some(Target::PaneContent("%1".to_owned())),
        );
    }

    #[test]
    fn a_pane_uri_without_the_sigil_names_nothing() {
        // Every tmux pane id starts with `%`. Accepting `1` would send a
        // lookup after something that cannot exist.
        assert_eq!(Target::parse("tmux://panes/1"), None);
    }

    #[test]
    fn a_session_name_with_a_space_arrives_intact() {
        assert_eq!(
            Target::parse("tmux://sessions/my%20project"),
            Some(Target::Session("my project".to_owned())),
        );
    }

    #[test]
    fn a_session_name_holding_a_percent_has_to_be_encoded() {
        // tmux allows `100%` as a session name, and a path segment must
        // encode a literal percent. Accepting the raw form would mean
        // guessing, and a session called `%25` would then be unreachable.
        assert_eq!(Target::parse("tmux://sessions/100%"), None);
        assert_eq!(
            Target::parse("tmux://sessions/100%25"),
            Some(Target::Session("100%".to_owned())),
        );
    }

    #[test]
    fn a_trailing_slash_names_nothing() {
        // `tmux://sessions/` is a listing with an empty name, not the
        // listing itself; answering it would make two URIs for one value.
        assert_eq!(Target::parse("tmux://sessions/"), None);
        assert_eq!(Target::parse("tmux://server/"), None);
        assert_eq!(Target::parse("tmux://panes/"), None);
    }

    #[test]
    fn uris_that_name_nothing_are_refused() {
        for uri in [
            "tmux://",
            "tmux://nope",
            "tmux://sessions/",
            "tmux://sessions/work/panes",
            "tmux://panes//content",
            "http://sessions",
            "tmux://panes/%1/content/extra",
        ] {
            assert_eq!(Target::parse(uri), None, "{uri} should name nothing");
        }
    }
}
