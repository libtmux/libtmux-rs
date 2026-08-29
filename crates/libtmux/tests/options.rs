//! Options and hooks against real tmux.

#![cfg(feature = "test-support")]
// Helpers outside a test function are not covered by clippy.toml's
// in-test exemptions, and these files have them.
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use libtmux::test::TestServer;
use libtmux::{EnvironmentEntry, OptionValue};
use libtmux::{NewWindowOptions, TmuxText};

fn bytes(value: Option<TmuxText>) -> Vec<u8> {
    value.expect("tmux reports a value").as_bytes().to_vec()
}

#[tokio::test]
async fn option_values_survive_bytes_that_tmux_would_quote_for_display() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // tmux renders each of these differently in its listing form: bare with
    // backslash escapes, double quotes, or single quotes. Reading through
    // `show-options -v` returns the stored bytes whatever the display form.
    for value in [
        "plain",
        "a b  c",
        "has\"quote",
        "has\\back",
        "has;semi",
        "has'single",
        "tab\tsep",
    ] {
        server
            .set_option("@probe", value)
            .await
            .expect("option is set");
        assert_eq!(
            bytes(server.get_option("@probe").await.expect("option is read")),
            value.as_bytes(),
            "{value:?} round-trips exactly",
        );
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn an_unknown_option_is_an_error_while_an_unset_one_is_absent() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // A user option exists only while it is set, so an unset one is absent
    // rather than an error, even though tmux itself calls it unknown.
    assert!(
        server
            .get_option("@absent")
            .await
            .expect("an unset user option is absent")
            .is_none(),
    );

    // A built-in name tmux does not have is a caller mistake.
    let error = server
        .get_option("no-such-built-in")
        .await
        .expect_err("an unknown built-in name is refused");
    assert!(matches!(
        error,
        libtmux::Error::OptionRejected {
            kind: libtmux::OptionErrorKind::Unknown,
            ..
        },
    ));

    // A known option with no value at this scope reports absence instead.
    assert!(
        server
            .get_global_option("after-kill-pane[0]")
            .await
            .expect("a known hook name is accepted")
            .is_none(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn options_are_scoped_to_the_object_that_set_them() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let session = server.new_session("scoped").await.expect("session created");
    let window = session
        .new_window(NewWindowOptions::new("scoped").command("sleep 300"))
        .await
        .expect("window created");
    let pane = window
        .panes()
        .await
        .expect("panes list")
        .into_iter()
        .next()
        .expect("one pane");

    server.set_option("@where", "server").await.expect("set");
    session.set_option("@where", "session").await.expect("set");
    window.set_option("@where", "window").await.expect("set");
    pane.set_option("@where", "pane").await.expect("set");

    assert_eq!(
        bytes(server.get_option("@where").await.expect("read")),
        b"server"
    );
    assert_eq!(
        bytes(session.get_option("@where").await.expect("read")),
        b"session"
    );
    assert_eq!(
        bytes(window.get_option("@where").await.expect("read")),
        b"window"
    );
    assert_eq!(
        bytes(pane.get_option("@where").await.expect("read")),
        b"pane"
    );

    // Unsetting one scope leaves the others alone.
    window.unset_option("@where").await.expect("unset");
    assert!(window.get_option("@where").await.expect("read").is_none());
    assert_eq!(
        bytes(session.get_option("@where").await.expect("read")),
        b"session"
    );
    assert_eq!(
        bytes(pane.get_option("@where").await.expect("read")),
        b"pane"
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn appending_extends_a_value_rather_than_replacing_it() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server
        .new_session("appending")
        .await
        .expect("session created");

    session.set_option("@parts", "one").await.expect("set");
    session
        .append_option("@parts", "-two")
        .await
        .expect("append");

    assert_eq!(
        bytes(session.get_option("@parts").await.expect("read")),
        b"one-two",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn option_names_lists_what_is_set_without_guessing_at_values() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    server
        .set_option("@listed", "a value with spaces")
        .await
        .expect("option is set");

    let names = server.option_names().await.expect("names are listed");
    assert!(names.iter().any(|name| name == "@listed"));
    // Array options appear once per index, exactly as tmux writes them.
    assert!(
        names.iter().any(|name| name.starts_with("command-alias[")),
        "array options keep their index",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_hook_is_stored_as_an_indexed_option() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    server
        .set_hook("after-new-window", "display-message hooked")
        .await
        .expect("hook is set");

    // Hooks live in the option tables, so the same reader sees them.
    assert_eq!(
        bytes(
            server
                .get_global_option("after-new-window[0]")
                .await
                .expect("hook is read"),
        ),
        b"display-message hooked",
    );

    server
        .unset_hook("after-new-window")
        .await
        .expect("hook is removed");
    assert!(
        server
            .get_global_option("after-new-window[0]")
            .await
            .expect("hook is read")
            .is_none(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn option_values_decode_into_flags_and_numbers() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    // Reading the global session table needs a session to exist, because tmux
    // resolves it against the current one.
    server.new_session("typed").await.expect("session");

    // tmux's own flag options read as flags.
    let status = server
        .get_global_option("status")
        .await
        .expect("read")
        .expect("status is set");
    assert_eq!(status.as_flag(), Some(true));

    // Numeric options parse without the caller checking UTF-8 first.
    // history-limit lives in the session table, not the server one.
    let limit = server
        .get_global_option("history-limit")
        .await
        .expect("read")
        .expect("history-limit is set");
    assert!(limit.parse::<u32>().is_some_and(|value| value > 0));

    // A value that is neither is reported as neither, rather than guessed at.
    server.set_option("@prose", "sometimes").await.expect("set");
    let prose = server
        .get_option("@prose")
        .await
        .expect("read")
        .expect("the option is set");
    assert_eq!(prose.as_flag(), None);
    assert_eq!(prose.parse::<u32>(), None);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn options_decode_by_declared_kind_rather_than_by_shape() {
    use libtmux::{OptionKind, OptionValue, option_schema};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    server.new_session("typed").await.expect("session");

    // The schema comes from tmux's own table, so a flag is a flag even though
    // its value is the text "off", and a number is a number.
    assert_eq!(
        option_schema("mouse").map(OptionSchemaKind::kind),
        Some(OptionKind::Flag),
    );
    assert_eq!(
        server.typed_global_option("mouse").await.expect("read"),
        Some(OptionValue::Flag(false)),
    );
    assert!(matches!(
        server
            .typed_global_option("history-limit")
            .await
            .expect("read"),
        Some(OptionValue::Number(limit)) if limit > 0,
    ));

    // `status` looks like a flag and is not: tmux accepts on, off, and 2
    // through 5 for several status lines. Guessing from the value would have
    // called it a flag, which is why the schema comes from tmux's own table.
    assert_eq!(
        option_schema("status").map(OptionSchemaKind::kind),
        Some(OptionKind::Choice),
    );
    assert!(matches!(
        server.typed_global_option("status").await.expect("read"),
        Some(OptionValue::Text(_)),
    ));

    // A user option has no declared type, so it stays text whatever it holds.
    assert_eq!(option_schema("@mine"), None);
    server.set_option("@mine", "on").await.expect("set");
    assert!(matches!(
        server.typed_option("@mine").await.expect("read"),
        Some(OptionValue::Text(_)),
    ));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

use libtmux::OptionSchema as OptionSchemaKind;

#[tokio::test]
async fn hooks_read_back_the_commands_they_were_set_to() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    assert!(
        server.hook("alert-bell").await.expect("read").is_none(),
        "a hook holding nothing is absent, not present and empty",
    );

    // A value tmux renders with quotes when it lists it, so reading it back
    // through the listing would return the rendering rather than the value.
    server
        .set_hook("alert-bell", r#"run-shell "echo a  b""#)
        .await
        .expect("the hook is set");

    let read = server
        .hook("alert-bell")
        .await
        .expect("read")
        .expect("the hook is set");
    assert_eq!(read.len(), 1);
    assert_eq!(
        read.first().expect("a command").as_bytes(),
        br#"run-shell "echo a  b""#,
        "the exact bytes come back, double space and all",
    );

    // The listing reports it too, and reports only hooks holding something:
    // tmux names every hook it knows and most of them hold nothing.
    let all = server.hooks().await.expect("listing");
    assert!(all.contains_key("alert-bell"));
    assert!(
        all.len() < 20,
        "only the hooks holding something are listed, not every name tmux knows: {}",
        all.len(),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_hook_keeps_the_indices_tmux_stores_it_under() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // Setting without an index writes slot 0; an explicit index writes that
    // slot, and the gap between them is real rather than closed up.
    server
        .set_hook("alert-bell", "display-message zero")
        .await
        .expect("slot 0");
    server
        .set_hook("alert-bell[3]", "display-message three")
        .await
        .expect("slot 3");

    let read = server
        .hook("alert-bell")
        .await
        .expect("read")
        .expect("the hook is set");

    assert_eq!(read.len(), 2);
    let indices: Vec<u32> = read.iter().map(|(index, _)| *index).collect();
    assert_eq!(indices, [0, 3], "the gap tmux keeps is kept here too");
    assert!(read.get(1).is_none(), "nothing is invented for the gap");
    assert_eq!(
        read.first()
            .map(|value| value.to_string_lossy().into_owned()),
        Some("display-message zero".to_owned()),
        "the lowest index is what a hook set without one holds",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_hook_set_on_a_window_is_read_back_by_name() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("scoped").await.expect("session");
    let window = session
        .active_window()
        .await
        .expect("active window")
        .expect("a session has a window");

    window
        .set_hook("alert-bell", "display-message window")
        .await
        .expect("window hook");

    // Reading by name is what works at every scope. tmux will not enumerate
    // the hooks set on a window or a pane -- `show-hooks` reports nothing for
    // them and `show-options` omits them while still listing ordinary options
    // -- but it does list the slots of a hook it is asked about by name. That
    // is why `Window` and `Pane` offer `hook` and no listing: a listing there
    // could answer nothing but empty, which reads as "none set".
    let read = window
        .hook("alert-bell")
        .await
        .expect("read")
        .expect("the window holds what was set on it");
    assert_eq!(
        read.first()
            .map(|value| value.to_string_lossy().into_owned()),
        Some("display-message window".to_owned()),
    );

    // The server enumerates the scopes tmux does report.
    server
        .set_hook("alert-silence", "display-message server")
        .await
        .expect("server hook");
    assert!(
        server
            .hooks()
            .await
            .expect("listing")
            .contains_key("alert-silence"),
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_typed_listing_decodes_every_option_it_reports() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("listed").await.expect("session");

    session
        .set_option("status-left-length", "30")
        .await
        .expect("a number");
    session.set_option("status", "on").await.expect("a flag");
    session
        .set_option("@marker", "kept")
        .await
        .expect("a user option");

    let options = session.options().await.expect("listing");

    // Each value comes back decoded by the kind tmux declares, not guessed
    // from how it looks: `status` holds "on" and is a choice rather than a
    // flag, because tmux also accepts 2 through 5 for it.
    assert_eq!(
        options.get("status-left-length"),
        Some(&OptionValue::Number(30)),
    );
    assert!(matches!(
        options.get("@marker"),
        Some(OptionValue::Text(text)) if text.as_bytes() == b"kept",
    ));
    assert!(options.contains_key("status"));

    // The listing reports what is set, and an empty answer would mean nothing
    // is set rather than that the listing failed: a refusal is an error.
    assert!(!options.is_empty());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_listing_keeps_the_indices_an_array_option_is_stored_under() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // `command-alias` is a server option and an array, so tmux lists it one
    // entry per index and the listing keeps those names rather than
    // collapsing them.
    let options = server.options().await.expect("listing");

    let aliased: Vec<&String> = options
        .keys()
        .filter(|name| name.starts_with("command-alias["))
        .collect();
    assert!(
        !aliased.is_empty(),
        "tmux ships command-alias entries: {:?}",
        options.keys().take(5).collect::<Vec<_>>(),
    );

    // A listing reports what is set at that scope, not what an object would
    // resolve to. A session that has set nothing of its own is empty even
    // though every option still has an effective value it inherits.
    let fresh = server
        .new_session("inheriting")
        .await
        .expect("session")
        .options()
        .await
        .expect("listing");
    assert!(
        fresh.is_empty(),
        "a session that set nothing has nothing of its own: {fresh:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn an_environment_listing_tells_a_removal_from_a_value() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard.server().new_session("env").await.expect("session");

    session
        .set_environment("EDITOR", "vi")
        .await
        .expect("a value");
    session
        .hide_environment("PAGER")
        .await
        .expect("a removal mark");

    let environment = session.environment_all().await.expect("listing");

    // tmux stores "hold this value" and "do not pass this on" as different
    // things, and neither is absence. Reporting both as absent would hide the
    // second, which is what a caller sets deliberately.
    assert!(matches!(
        environment.get("EDITOR"),
        Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"vi",
    ));
    assert_eq!(environment.get("PAGER"), Some(&EnvironmentEntry::Removed));
    assert_eq!(environment.get("NEVER_SET"), None);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn an_environment_value_survives_what_a_line_listing_would_split() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard
        .server()
        .new_session("awkward")
        .await
        .expect("session");

    // Each of these breaks a naive parse of the listing: an embedded newline
    // spans lines, and a continuation line holding `=` reads exactly like the
    // next variable. Reading every name back on its own is what survives it.
    session
        .set_environment("MULTILINE", "first\nDECOY=not-a-variable")
        .await
        .expect("a value with a newline");
    session
        .set_environment("SPACED", "x  y")
        .await
        .expect("a value with runs of spaces");

    let environment = session.environment_all().await.expect("listing");

    assert!(matches!(
        environment.get("MULTILINE"),
        Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"first\nDECOY=not-a-variable",
    ));
    assert!(matches!(
        environment.get("SPACED"),
        Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"x  y",
    ));
    assert_eq!(
        environment.get("DECOY"),
        None,
        "a continuation line is not a variable, however much it looks like one",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn writing_a_whole_hook_replaces_or_merges_as_asked() {
    use libtmux::{IndexedHooks, ReplaceMode, TmuxText};
    use std::collections::BTreeMap;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    server
        .set_hook("alert-bell[7]", "display-message existing")
        .await
        .expect("an entry to survive or not");

    let mut entries = BTreeMap::new();
    entries.insert(0, TmuxText::from(b"display-message first".to_vec()));
    entries.insert(3, TmuxText::from(b"display-message fourth".to_vec()));
    let written = IndexedHooks::from(entries);

    // Merge leaves what it does not name, so the earlier entry stays.
    server
        .set_hooks("alert-bell", &written, ReplaceMode::Merge)
        .await
        .expect("merge");
    let merged = server
        .hook("alert-bell")
        .await
        .expect("read")
        .expect("the hook is set");
    assert_eq!(merged.len(), 3);
    assert!(
        merged.get(7).is_some(),
        "merge kept the entry it did not name"
    );

    // Replace clears first, so only what was written remains -- gaps and all.
    server
        .set_hooks("alert-bell", &written, ReplaceMode::Replace)
        .await
        .expect("replace");
    let replaced = server
        .hook("alert-bell")
        .await
        .expect("read")
        .expect("the hook is set");
    assert_eq!(
        replaced.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
        [0, 3],
        "the sparse indices are kept and nothing else survives",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_later_bulk_hook_refusal_reports_the_hook_already_written() {
    use libtmux::{Error, ErrorKind, IndexedHooks, ReplaceMode, TmuxText};
    use std::collections::BTreeMap;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let mut entries = BTreeMap::new();
    entries.insert(0, TmuxText::from("display-message first"));
    entries.insert(1, TmuxText::from("no-such-tmux-command"));

    let error = server
        .set_hooks(
            "alert-bell",
            &IndexedHooks::from(entries),
            ReplaceMode::Merge,
        )
        .await
        .expect_err("tmux refuses the second hook command");

    let hooks = server
        .hook("alert-bell")
        .await
        .expect("the partly written hook can be read")
        .expect("the first hook remains set");
    assert_eq!(
        hooks.get(0).map(TmuxText::as_bytes),
        Some(b"display-message first".as_slice()),
    );
    assert!(hooks.get(1).is_none(), "the refused hook was not stored");

    assert_eq!(error.kind(), ErrorKind::PartialEffect);
    assert!(matches!(
        error,
        Error::AfterEffect {
            operation: "set-hooks",
            source,
            ..
        } if source.kind() == ErrorKind::Refused
    ));

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_first_bulk_hook_refusal_has_no_partial_effect() {
    use libtmux::{ErrorKind, IndexedHooks, ReplaceMode, TmuxText};
    use std::collections::BTreeMap;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let mut entries = BTreeMap::new();
    entries.insert(0, TmuxText::from("no-such-tmux-command"));
    entries.insert(1, TmuxText::from("display-message never-written"));

    let error = server
        .set_hooks(
            "alert-bell",
            &IndexedHooks::from(entries),
            ReplaceMode::Merge,
        )
        .await
        .expect_err("tmux refuses the first hook command");

    assert_eq!(error.kind(), ErrorKind::Refused);
    assert!(
        server
            .hook("alert-bell")
            .await
            .expect("the hook can be read")
            .is_none(),
        "the member after the refusal never ran",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn tmux_refusing_an_option_says_which_of_three_things_went_wrong() {
    use libtmux::OptionErrorKind;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // Three different fixes: a typo, a name that needs more of itself, and a
    // value the option will not hold. Reading stderr to tell them apart also
    // means knowing tmux says "bad value" for a flag and "value is invalid"
    // for a number, which is not a distinction a caller should have to learn.
    let cases = [
        ("no-such-option", "x", OptionErrorKind::Unknown),
        ("status-l", "x", OptionErrorKind::Ambiguous),
        ("mouse", "notabool", OptionErrorKind::BadValue),
        (
            "status-left-length",
            "notanumber",
            OptionErrorKind::BadValue,
        ),
    ];

    for (name, value, expected) in cases {
        let error = server
            .set_global_option(name, value)
            .await
            .expect_err("tmux refuses it");
        assert!(
            matches!(&error, libtmux::Error::OptionRejected { kind, .. } if *kind == expected),
            "{name}={value} should be {expected:?}, got {error:?}",
        );
        assert_eq!(error.kind(), libtmux::ErrorKind::Refused);
    }

    // A name it does resolve is still accepted.
    server
        .set_global_option("status-left-length", "30")
        .await
        .expect("a valid option and value");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn rejected_sensitive_values_are_absent_from_errors() {
    use libtmux::{Error, OptionErrorKind};

    const SECRET: &str = "libtmux-sentinel-sensitive-value";

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    for name in ["mouse", "status-keys"] {
        let error = server
            .set_global_option(name, SECRET)
            .await
            .expect_err("tmux refuses the value");
        assert!(
            matches!(
                &error,
                Error::OptionRejected {
                    kind: OptionErrorKind::BadValue,
                    detail,
                } if detail == name
            ),
            "the error names the option and classifies its value: {error:?}",
        );
        assert!(!error.to_string().contains(SECRET), "{error}");
        assert!(!format!("{error:?}").contains(SECRET), "{error:?}");
    }

    let error = server
        .set_hook("after-new-window", SECRET)
        .await
        .expect_err("tmux refuses an unknown hook command");
    assert!(error.to_string().contains("set-hook"), "{error}");
    assert!(!error.to_string().contains(SECRET), "{error}");
    assert!(!format!("{error:?}").contains(SECRET), "{error:?}");

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn the_server_environment_is_separate_from_every_session_one() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    server
        .set_environment("SERVER_ONLY", "yes")
        .await
        .expect("a server value");

    // Separate stores, not one layered over the other. Reading a session is
    // not a fallback to the server, whenever the session was created: tmux
    // merges the two only when it starts a process.
    let before = server.new_session("before").await.expect("session");
    let after = server.new_session("after").await.expect("session");
    for (label, session) in [("before", &before), ("after", &after)] {
        assert_eq!(
            session.environment("SERVER_ONLY").await.expect("read"),
            None,
            "the {label} session has no entry of its own for a server variable",
        );
    }

    // Setting it on one session does not reach back to the server.
    after
        .set_environment("SESSION_ONLY", "mine")
        .await
        .expect("a session value");
    assert_eq!(
        server.environment("SESSION_ONLY").await.expect("read"),
        None
    );

    // The two halves of removal behave on the server as they do on a session.
    server.hide_environment("HIDDEN").await.expect("a mark");
    assert_eq!(
        server.environment("HIDDEN").await.expect("read"),
        Some(EnvironmentEntry::Removed),
    );

    let listing = server.environment_all().await.expect("listing");
    assert!(matches!(
        listing.get("SERVER_ONLY"),
        Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"yes",
    ));
    assert_eq!(listing.get("HIDDEN"), Some(&EnvironmentEntry::Removed));
    assert_eq!(listing.get("SESSION_ONLY"), None);

    server
        .unset_environment("SERVER_ONLY")
        .await
        .expect("removal");
    assert_eq!(server.environment("SERVER_ONLY").await.expect("read"), None);

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_started_process_gets_the_server_and_session_environments_merged() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    server
        .set_environment("BOTH", "from_server")
        .await
        .expect("set");
    server
        .set_environment("ONLY_SERVER", "reaches")
        .await
        .expect("set");
    server
        .hide_environment("HIDDEN_GLOBALLY")
        .await
        .expect("mark");

    let session = server.new_session("merged").await.expect("session");
    session
        .set_environment("BOTH", "from_session")
        .await
        .expect("set");

    // The stores are separate right up until tmux starts something, so this is
    // the only place the merge is observable. Reading the two stores back tells
    // a caller nothing about what a pane will actually be handed.
    let window = session
        .new_window(
            NewWindowOptions::new("merged")
                .command("sh -c 'printf \"%s|%s|%s\" \"$BOTH\" \"$ONLY_SERVER\" \"${HIDDEN_GLOBALLY-absent}\"; sleep 30'"),
        )
        .await
        .expect("window created");
    let pane = window
        .panes()
        .await
        .expect("panes")
        .into_iter()
        .next()
        .expect("one pane");

    let mut captured = String::new();
    for _ in 0..50 {
        let lines = pane.capture().await.expect("capture");
        captured = lines
            .iter()
            .map(|line| line.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        if captured.contains('|') {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }

    // The session's value wins, a server-only name still arrives, and a
    // globally hidden name is missing rather than empty.
    assert!(
        captured.contains("from_session|reaches|absent"),
        "merged environment, got {captured:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn an_array_option_keeps_the_gaps_tmux_leaves() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    // tmux ships defaults in the low indices of this option, so writing far
    // above them keeps the test's own entries distinguishable.
    server
        .set_array_option("command-alias", 30, "thirty=display -p 30")
        .await
        .expect("index 30 is set");
    server
        .set_array_option("command-alias", 35, "five=display -p 35")
        .await
        .expect("index 35 is set");

    let aliases = server.array_option("command-alias").await.expect("read");
    assert_eq!(
        bytes(aliases.get(30).cloned()),
        b"thirty=display -p 30",
        "the value is stored under the index asked for",
    );
    assert_eq!(aliases.get(31), None, "nothing was written between them");
    assert!(
        aliases.indices().any(|index| *index == 35),
        "the higher index is listed: {:?}",
        aliases.indices().collect::<Vec<_>>(),
    );

    // Appending extends that index's value rather than adding an entry.
    let before = aliases.len();
    server
        .append_array_option("command-alias", 30, " extra")
        .await
        .expect("append");
    let appended = server.array_option("command-alias").await.expect("read");
    assert_eq!(appended.len(), before, "appending did not add an entry");
    assert_eq!(
        bytes(appended.get(30).cloned()),
        b"thirty=display -p 30 extra",
    );

    // Removing one leaves the rest where they were: nothing renumbers.
    server
        .unset_array_option("command-alias", 35)
        .await
        .expect("unset");
    let after = server.array_option("command-alias").await.expect("read");
    assert_eq!(after.get(35), None, "the entry is gone");
    assert_eq!(
        bytes(after.get(30).cloned()),
        b"thirty=display -p 30 extra",
        "the entry below it did not move",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_name_shaped_like_a_flag_is_refused_not_obeyed() {
    // tmux reads a leading `-` as a flag wherever it appears, so an option
    // name a caller did not write could act on a different option. `-u` is
    // the sharp one: it unsets.
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("flags").await.expect("session");

    session
        .set_option("@kept", "original")
        .await
        .expect("a plain user option is set");

    session
        .set_option("-u", "@kept")
        .await
        .expect_err("a name that is a flag is refused");

    assert_eq!(
        bytes(session.get_option("@kept").await.expect("the option reads")),
        b"original".to_vec(),
        "the option a flag name pointed at survived",
    );

    // A variable may legitimately be named anything, so the guard does not
    // refuse this one; what it stops is `-u` being read as the flag that
    // removes a different variable.
    session
        .set_environment("KEPT", "original")
        .await
        .expect("a plain variable is set");
    session
        .set_environment("-u", "KEPT")
        .await
        .expect("a variable named like a flag is a variable");

    let kept = session
        .environment("KEPT")
        .await
        .expect("the variable reads");
    assert!(
        matches!(&kept, Some(EnvironmentEntry::Set(value)) if value.as_bytes() == b"original"),
        "the variable a flag name pointed at survived, got {kept:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_whole_hook_write_refuses_bytes_rather_than_substituting_them() {
    use libtmux::{IndexedHooks, ReplaceMode, TmuxText};
    use std::collections::BTreeMap;

    // A hook value is a tmux command, and tmux refuses one carrying a byte it
    // cannot read. Passing the bytes through a `String` first replaced that
    // byte with U+FFFD, which tmux then accepted: the caller was told the
    // write succeeded and tmux held a command they had not written.
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    let mut command = b"set-environment -g MARK \"a".to_vec();
    command.push(0xff);
    command.extend_from_slice(b"b\"");

    let mut entries = BTreeMap::new();
    entries.insert(0, TmuxText::from(command));
    server
        .set_hooks(
            "alert-bell",
            &IndexedHooks::from(entries),
            ReplaceMode::Replace,
        )
        .await
        .expect_err("tmux refuses a command it cannot read");

    assert!(
        server
            .hook("alert-bell")
            .await
            .expect("the hook reads")
            .is_none_or(|hook| hook.get(0).is_none()),
        "nothing was stored under a substituted value",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn setting_one_hook_leaves_the_other_slots_alone() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();

    for slot in [0_usize, 2, 5] {
        server
            .set_hook(
                &format!("alert-bell[{slot}]"),
                format!("display-message {slot}"),
            )
            .await
            .expect("a slot is written");
    }

    // A hook is an array, and tmux empties the array for an unindexed write.
    // Setting one hook used to discard every other slot the caller had
    // registered, and report success for doing it.
    server
        .set_hook("alert-bell", "display-message replaced")
        .await
        .expect("the hook is set");

    let hooks = server
        .hook("alert-bell")
        .await
        .expect("the hook reads")
        .expect("the hook is set");
    assert_eq!(
        hooks.get(2).map(|command| command.to_string_lossy()),
        Some("display-message 2".into()),
        "a slot nobody wrote to survives: {hooks:?}",
    );
    assert_eq!(
        hooks.get(5).map(|command| command.to_string_lossy()),
        Some("display-message 5".into()),
        "and so does the last one: {hooks:?}",
    );
    assert_eq!(
        hooks.get(0).map(|command| command.to_string_lossy()),
        Some("display-message replaced".into()),
        "while slot 0 is the one that changed: {hooks:?}",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn the_schema_agrees_with_where_tmux_accepts_a_write() {
    use libtmux::{OptionScope, option_schema};

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let server = guard.server();
    let session = server.new_session("scoped").await.expect("a session");
    let window = session
        .windows()
        .await
        .expect("windows")
        .into_iter()
        .next()
        .expect("one window");

    // A pane hook is a window option too, and tmux has said so since 3.2a. The
    // schema called it pane-only, so anything consulting it to choose a scope
    // was told this write would not land.
    let schema = option_schema("pane-died").expect("a documented hook");
    assert!(schema.accepts(OptionScope::Window), "{:?}", schema.scopes());

    window
        .set_hook("pane-died", "display-message gone")
        .await
        .expect("tmux accepts a pane hook at window scope");
    assert!(
        window
            .hook("pane-died")
            .await
            .expect("the hook reads")
            .is_some(),
        "and reads it back there",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}
