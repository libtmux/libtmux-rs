use super::*;

use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::Path;
use std::process::{Command, Stdio};

const FRAME_PREFIX: &str = "__LIBTMUX_MCP_DONE_";

#[test]
fn finding_a_needle_reports_where_it_starts() {
    assert_eq!(find(b"abcdef", b"cd"), Some(2));
    assert_eq!(find(b"abcdef", b"xy"), None);
    assert_eq!(find(b"ab", b"abcdef"), None);
    assert_eq!(find(b"abc", b""), None);
}

#[test]
fn shell_words_preserve_raw_bytes_and_split_apostrophes() {
    for (input, expected) in [
        (b"".as_slice(), b"''".as_slice()),
        (b"plain", b"'plain'"),
        (b"a'b", b"'a'\\''b'"),
        (b"line\n\xff", b"'line\n\xff'"),
    ] {
        let input = OsString::from_vec(input.to_vec());
        assert_eq!(quote_shell_word(&input).as_bytes(), expected);
    }
}

#[test]
fn route_words_reject_only_ascii_terminal_control_bytes() {
    for byte in 0_u8..=u8::MAX {
        let value = OsString::from_vec(vec![b'x', byte]);
        assert_eq!(
            route_path_is_terminal_safe(&value),
            !(byte <= 0x1f || byte == 0x7f),
            "byte {byte:#04x}"
        );
    }
}

#[test]
fn terminal_control_routes_fail_before_entropy() {
    for (executable, socket) in [
        (
            OsString::from_vec(b"tmux-\x03".to_vec()),
            OsString::from("s"),
        ),
        (
            OsString::from("tmux"),
            OsString::from_vec(b"s-\x03".to_vec()),
        ),
    ] {
        let mut entropy_called = false;
        let error = frame_with_random(
            &executable,
            Path::new(&socket),
            b"sh",
            OsStr::new("true"),
            false,
            |bytes| {
                entropy_called = true;
                bytes.fill(0);
                Ok(())
            },
        )
        .err()
        .unwrap_or_else(|| unreachable!("terminal control must stop framing"));

        assert!(matches!(error, FrameError::TerminalControl));
        assert!(!entropy_called, "route validation precedes frame creation");
    }
}

#[test]
fn rendered_frame_is_raw_variable_free_and_posix_syntax() {
    let executable = OsString::from_vec(b"/tmp/tmux-'\xff".to_vec());
    let socket = OsString::from_vec(b"/tmp/socket-'\xfe".to_vec());
    let payload = render_payload(
        &executable,
        Path::new(&socket),
        "nonce",
        b"sh",
        OsStr::new("printf body # trailing comment"),
        false,
    );
    let bytes = payload.as_bytes();

    assert_eq!(
        find(bytes, executable.as_bytes()),
        None,
        "raw paths are shell quoted"
    );
    assert_eq!(
        bytes
            .windows(b"run-shell".len())
            .filter(|w| *w == b"run-shell")
            .count(),
        0
    );
    assert_eq!(
        bytes
            .windows(b"display-message".len())
            .filter(|w| *w == b"display-message")
            .count(),
        16
    );
    assert_eq!(find(bytes, b"__LIBTMUX_MCP_DONE_nonce__"), None);
    assert_eq!(
        bytes
            .windows(b"\\set -x\n".len())
            .filter(|w| *w == b"\\set -x\n")
            .count(),
        2
    );
    for forbidden in [
        b"__tmux_mcp".as_slice(),
        b"/usr/bin/printf",
        b"command printf",
    ] {
        assert_eq!(find(bytes, forbidden), None, "forbidden bookkeeping token");
    }
    assert!(
        find(bytes, b"'\\''").is_some(),
        "apostrophes are split without loss"
    );
    assert!(find(bytes, b"# trailing comment'").is_some());

    let mut child = Command::new("/bin/sh")
        .arg("-n")
        .stdin(Stdio::piped())
        .spawn()
        .expect("POSIX shell starts");
    child
        .stdin
        .take()
        .expect("syntax checker stdin")
        .write_all(bytes)
        .expect("payload is written");
    assert!(child.wait().expect("syntax checker exits").success());
}

#[test]
fn rendered_frame_matches_the_exact_four_branch_snapshot() {
    let actual = render_payload(
        OsStr::new("/tmp/tmux"),
        Path::new("/tmp/socket"),
        "nonce",
        b"sh",
        OsStr::new("printf body # trailing comment"),
        false,
    );
    let separator = r"( \exec '/tmp/tmux' -N -S '/tmp/socket' display-message -p '' )";
    let opening = r"( \exec '/tmp/tmux' -N -S '/tmp/socket' display-message -p '__LIBTMUX_MCP_DONE_''nonce''__:BEGIN' )";
    let closing = r#"( \exec '/tmp/tmux' -N -S '/tmp/socket' display-message -p '__LIBTMUX_MCP_DONE_''nonce''__:'"$1" )"#;
    let expected = format!(
        r#"(
case $- in
*x*)
\set +x
case $- in
*e*)
\set +e
if {separator} && {opening}; then
( \set -e; \eval '\set -x
printf body # trailing comment' )
\set -- "$?"
{separator}
{closing}
fi
;;
*)
\set +e
if {separator} && {opening}; then
( \set +e; \eval '\set -x
printf body # trailing comment' )
\set -- "$?"
{separator}
{closing}
fi
;;
esac
;;
*)
case $- in
*e*)
\set +e
if {separator} && {opening}; then
( \set -e; \eval 'printf body # trailing comment' )
\set -- "$?"
{separator}
{closing}
fi
;;
*)
\set +e
if {separator} && {opening}; then
( \set +e; \eval 'printf body # trailing comment' )
\set -- "$?"
{separator}
{closing}
fi
;;
esac
;;
esac
)"#
    );

    assert_eq!(actual.as_bytes(), expected.as_bytes());
    assert_eq!(
        actual,
        render_payload(
            OsStr::new("/tmp/tmux"),
            Path::new("/tmp/socket"),
            "nonce",
            b"dash",
            OsStr::new("printf body # trailing comment"),
            false,
        ),
        "dash retains the original POSIX frame"
    );
}

#[test]
fn trap_capture_is_bounded_and_preserves_foreign_descriptors() {
    for (shell, flags) in [
        ("/bin/bash", ["--noprofile", "--norc"].as_slice()),
        ("/bin/zsh", ["-f"].as_slice()),
    ] {
        if !Path::new(shell).is_file() {
            continue;
        }
        let shell_name = shell.rsplit('/').next().unwrap_or("shell");
        for (index, (case, action, expected_status, fd8_open)) in [
            ("ordinary", ": # quoted\n:".to_owned(), 0, false),
            (
                "oversized",
                format!(
                    "__libtmux_mcp_large='{}'",
                    "x".repeat(TRAP_DECLARATION_LIMIT + 16)
                ),
                125,
                false,
            ),
            ("occupied", ": # occupied".to_owned(), 125, true),
        ]
        .into_iter()
        .enumerate()
        {
            let nonce = format!("unit{}{}{}", std::process::id(), shell_name, index);
            let capture = inherited_trap_capture(shell_name.as_bytes(), &nonce)
                .unwrap_or_else(|| unreachable!("tested shells capture traps"));
            let mut script = String::new();
            if fd8_open {
                script.push_str("exec 8>/dev/null; ");
            }
            script.push_str("trap ");
            script.push_str(&quote_shell_word(OsStr::new(&action)).to_string_lossy());
            script.push_str(" DEBUG; ");
            script.push_str(std::str::from_utf8(&capture.setup).expect("capture setup is ASCII"));
            let _ = write!(
                script,
                "[ \"${}\" -eq {expected_status} ] || exit 80; ",
                capture.status
            );
            script.push_str(if fd8_open {
                "( : >&8 ) 2>/dev/null || exit 81; "
            } else {
                "! ( : >&8 ) 2>/dev/null || exit 81; "
            });
            script
                .push_str("! ( : <&9 ) 2>/dev/null || exit 82; ! ( : >&9 ) 2>/dev/null || exit 83");

            let output = Command::new(shell)
                .args(flags)
                .arg("-c")
                .arg(script)
                .output()
                .unwrap_or_else(|error| panic!("{shell_name}/{case} starts: {error}"));
            assert!(
                output.status.success(),
                "{shell_name}/{case}: status={:?}, stdout={:?}, stderr={:?}",
                output.status.code(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let prefix = format!("libtmux-mcp-traps-{nonce}.");
            assert!(
                std::fs::read_dir("/tmp")
                    .expect("temporary directory is readable")
                    .all(|entry| !entry
                        .expect("temporary entry is readable")
                        .file_name()
                        .to_string_lossy()
                        .starts_with(&prefix)),
                "{shell_name}/{case} left a trap-capture file"
            );
        }
    }
}

#[test]
fn random_frames_retry_source_collisions_with_128_bit_nonces() {
    let collision = format!("{FRAME_PREFIX}{}__", "00".repeat(16));
    let mut calls = 0;
    let frame = frame_with_random(
        OsStr::new("/tmp/tmux"),
        Path::new("/tmp/socket"),
        b"sh",
        OsStr::new(&collision),
        false,
        |bytes| {
            bytes.fill(if calls == 0 { 0 } else { 0x11 });
            calls += 1;
            Ok(())
        },
    )
    .unwrap_or_else(|_| unreachable!("the second nonce is collision-free"));

    assert_eq!(calls, 2);
    let marker_len = FRAME_PREFIX.len() + 32 + 2;
    assert_eq!(frame.opened.len(), marker_len + ":BEGIN".len());
    assert_eq!(
        find(frame.payload.as_bytes(), &frame.opened[..marker_len]),
        None
    );
}

#[test]
fn random_frame_entropy_failure_is_preflight_failure() {
    let error = frame_with_random(
        OsStr::new("/tmp/tmux"),
        Path::new("/tmp/socket"),
        b"sh",
        OsStr::new("true"),
        false,
        |_| Err(getrandom::Error::UNSUPPORTED),
    )
    .err()
    .unwrap_or_else(|| unreachable!("entropy failure must stop framing"));

    assert!(matches!(error, FrameError::Entropy));
}

#[test]
fn repeated_random_marker_collisions_are_bounded() {
    let collision = format!("{FRAME_PREFIX}{}__", "00".repeat(16));
    let mut calls = 0;
    let error = frame_with_random(
        OsStr::new("/tmp/tmux"),
        Path::new("/tmp/socket"),
        b"sh",
        OsStr::new(&collision),
        false,
        |bytes| {
            bytes.fill(0);
            calls += 1;
            Ok(())
        },
    )
    .err()
    .unwrap_or_else(|| unreachable!("every candidate collides"));

    assert!(matches!(error, FrameError::Collisions));
    assert_eq!(calls, 32);
}

#[test]
fn a_literal_pattern_is_not_a_regular_expression() {
    let patterns = Patterns::compile(&["a.c".to_owned()], false, true)
        .unwrap_or_else(|_| unreachable!("a literal always compiles"));

    assert!(patterns.first_match(b"a.c").is_some());
    assert!(
        patterns.first_match(b"abc").is_none(),
        "the dot must match itself when the caller asked for literal text"
    );
}

#[test]
fn a_regular_expression_is_one_when_asked() {
    let patterns = Patterns::compile(&["a.c".to_owned()], true, true)
        .unwrap_or_else(|_| unreachable!("a valid expression compiles"));

    assert!(patterns.first_match(b"abc").is_some());
}

#[test]
fn matching_ignores_case_unless_asked() {
    let insensitive = Patterns::compile(&["DONE".to_owned()], false, false)
        .unwrap_or_else(|_| unreachable!("a literal always compiles"));
    let sensitive = Patterns::compile(&["DONE".to_owned()], false, true)
        .unwrap_or_else(|_| unreachable!("a literal always compiles"));

    assert!(insensitive.first_match(b"done").is_some());
    assert!(sensitive.first_match(b"done").is_none());
}

#[test]
fn the_first_pattern_given_is_the_one_reported() {
    let patterns = Patterns::compile(&["one".to_owned(), "two".to_owned()], false, true)
        .unwrap_or_else(|_| unreachable!("literals always compile"));

    assert_eq!(patterns.first_match(b"two one"), Some((0, "one")));
}

#[test]
fn a_bad_expression_names_itself() {
    let error = Patterns::compile(&["a(".to_owned()], true, true);
    let (source, _reason) = error
        .err()
        .unwrap_or_else(|| unreachable!("`a(` is invalid"));

    assert_eq!(source, "a(");
}

#[test]
fn an_invalid_literal_is_still_a_literal() {
    // `a(` is not a valid expression, but as text it is ordinary.
    let patterns = Patterns::compile(&["a(".to_owned()], false, true)
        .unwrap_or_else(|_| unreachable!("escaping makes any text valid"));

    assert!(patterns.first_match(b"a(").is_some());
}

#[test]
fn pattern_size_is_bounded() {
    let error = Patterns::compile(&["x".repeat(4097)], false, true)
        .err()
        .unwrap_or_else(|| unreachable!("an oversized literal is rejected"));

    assert!(error.1.contains("4096"), "{}", error.1);
}

#[test]
fn pattern_count_is_bounded() {
    let patterns = vec!["x".to_owned(); 33];
    let error = Patterns::compile(&patterns, false, true)
        .err()
        .unwrap_or_else(|| unreachable!("too many patterns are rejected"));

    assert!(error.1.contains("32"), "{}", error.1);
}

#[test]
fn aggregate_pattern_size_is_bounded() {
    let patterns = vec!["x".repeat(4096); 5];
    let error = Patterns::compile(&patterns, false, true)
        .err()
        .unwrap_or_else(|| unreachable!("oversized aggregate patterns are rejected"));

    assert!(error.1.contains("16384"), "{}", error.1);
}

/// Feed a scanner one run's stream, split at the given byte offsets.
fn scan(stream: &[u8], splits: &[usize]) -> Option<RunView> {
    let mut scanner = scanner();
    let mut at = 0;
    let mut finished = None;
    for &next in splits.iter().chain(std::iter::once(&stream.len())) {
        let chunk = &stream[at..next];
        at = next;
        finished = finished.or_else(|| scanner.push(chunk));
    }
    finished
}

const MARKER: &[u8] = b"__LIBTMUX_MCP_DONE_0123456789abcdef0123456789abcdef__";

fn scanner() -> Scanner {
    let mut opened = MARKER.to_vec();
    opened.extend_from_slice(b":BEGIN");
    let mut closed = MARKER.to_vec();
    closed.push(b':');
    Scanner::new(opened, closed)
}

fn one_run() -> Vec<u8> {
    let mut stream = Vec::new();
    stream.extend_from_slice(
        br"display-message -p '__LIBTMUX_MCP_DONE_''0123456789abcdef0123456789abcdef''__:BEGIN'",
    );
    stream.extend_from_slice(b"\r\n\r\n");
    stream.extend_from_slice(MARKER);
    stream.extend_from_slice(b":BEGIN\r\nhi\r\n\r\n");
    stream.extend_from_slice(MARKER);
    stream.extend_from_slice(b":42\r\n");
    stream
}

#[test]
fn exact_physical_completion_lines_report_status() {
    let view = scan(&one_run(), &[]).unwrap_or_else(|| unreachable!("the run completed"));

    assert_eq!(view.exit_status, Some(42));
    assert_eq!(view.output, "hi\n");
}

#[test]
fn scanner_publishes_state_at_a_trimmed_body_start() {
    let mut scanner = scanner();
    let mut body = b"\x1b[31mred".to_vec();
    body.resize(OUTPUT_LIMIT + 4, b'x');
    let mut stream = MARKER.to_vec();
    stream.extend_from_slice(b":BEGIN\n");
    stream.extend_from_slice(&body);

    assert!(scanner.push(&stream).is_none());
    let progress = scanner.progress();
    assert_eq!(progress.body_dropped, 4);
    let retained = progress
        .body
        .and_then(|range| scanner.retained().get(range))
        .unwrap_or_default();

    let text = readable_from(progress.body_checkpoint, retained, 0);

    assert!(text.starts_with("red"));
    assert_eq!(text.len(), OUTPUT_LIMIT - 1);

    let closed = scanner.unfinished(RunOutcome::PaneClosed, "%0".to_owned());
    assert!(closed.output.starts_with("red"));
}

#[test]
fn scanner_publishes_each_retained_byte_once() {
    const CHUNK: usize = 1024;
    let mut scanner = Scanner::new(b"open".to_vec(), b"close".to_vec());
    let chunk = [b'x'; CHUNK];
    let chunks = OUTPUT_LIMIT / CHUNK + 64;
    let mut published = 0;
    let mut mirror = RetainedBytes::new();

    for _ in 0..chunks {
        assert!(scanner.push(&chunk).is_none());
        let progress = scanner.progress();
        published += progress.publication_bytes();
        mirror.discard(progress.discarded);
        mirror.append(progress.appended);
        assert_eq!(mirror.as_slice(), scanner.retained());
    }

    assert_eq!(published, chunks * CHUNK);
}

#[test]
fn scanner_compacts_dropped_storage_in_batches() {
    const CHUNK: usize = 1024;
    let mut scanner = Scanner::new(b"open".to_vec(), b"close".to_vec());
    let chunk = [b'x'; CHUNK];

    for _ in 0..OUTPUT_LIMIT / CHUNK + 32 {
        assert!(scanner.push(&chunk).is_none());
    }
    assert_eq!(scanner.retained().len(), OUTPUT_LIMIT);
    assert!(scanner.physical_bytes() > OUTPUT_LIMIT);

    let mut previous = scanner.physical_bytes();
    let mut compacted = false;
    for _ in 0..COMPACT_AFTER / CHUNK {
        assert!(scanner.push(&chunk).is_none());
        let current = scanner.physical_bytes();
        compacted |= current < previous;
        previous = current;
    }
    assert_eq!(scanner.retained().len(), OUTPUT_LIMIT);
    assert!(compacted);
    assert!(scanner.physical_bytes() <= OUTPUT_LIMIT + COMPACT_AFTER);
}

#[test]
fn scanner_releases_an_oversized_chunk_allocation() {
    let mut scanner = Scanner::new(b"open".to_vec(), b"close".to_vec());
    let chunk = vec![b'x'; OUTPUT_LIMIT * 4];

    assert!(scanner.push(&chunk).is_none());

    assert_eq!(scanner.retained().len(), OUTPUT_LIMIT);
    assert!(scanner.physical_capacity() <= OUTPUT_LIMIT + COMPACT_AFTER);
    assert!(scanner.frame_line_capacity() <= 128);
}

#[test]
fn an_incomplete_closing_line_does_not_suspend_trimming() {
    let mut scanner = scanner();
    let mut chunk = MARKER.to_vec();
    chunk.extend_from_slice(b":BEGIN\n");
    chunk.resize(OUTPUT_LIMIT + 32, b'x');
    chunk.extend_from_slice(MARKER);
    chunk.extend_from_slice(b":12");

    assert!(scanner.push(&chunk).is_none());

    assert_eq!(scanner.retained().len(), OUTPUT_LIMIT);
}

#[test]
fn completed_output_resumes_at_the_trim_checkpoint() {
    let ending = [b"\n".as_slice(), MARKER, b":0\n".as_slice()].concat();
    let mut body = b"\x1b[31mred".to_vec();
    body.resize(OUTPUT_LIMIT + 4 - ending.len(), b'x');
    let mut stream = MARKER.to_vec();
    stream.extend_from_slice(b":BEGIN\n");
    stream.extend_from_slice(&body);
    stream.extend_from_slice(&ending);
    let mut scanner = scanner();

    let view = scanner
        .push(&stream)
        .unwrap_or_else(|| unreachable!("the completed run is whole"));

    assert!(
        view.output.starts_with("red"),
        "output prefix was {:?}",
        view.output.get(..4),
    );
}

#[test]
fn a_run_split_between_its_marker_and_its_status_is_still_read() {
    // tmux decides where a chunk ends. Splitting immediately after the
    // closing prefix leaves the status digits for a later chunk.
    let stream = one_run();
    let after_marker = stream.len() - "42\r\n".len();

    let view = scan(&stream, &[after_marker])
        .unwrap_or_else(|| unreachable!("a split chunk must not lose the run"));

    assert_eq!(view.exit_status, Some(42));
    assert_eq!(view.output, "hi\n");
}

#[test]
fn a_run_split_at_every_byte_is_still_read() {
    let stream = one_run();
    let splits: Vec<usize> = (1..stream.len()).collect();

    let view = scan(&stream, &splits)
        .unwrap_or_else(|| unreachable!("no chunk boundary may lose the run"));

    assert_eq!(view.exit_status, Some(42));
    assert_eq!(view.output, "hi\n");
}

#[test]
fn opening_record_separators_are_not_command_output() {
    for separator in [b"\n".as_slice(), b"\r\n"] {
        let mut stream = b"echo source\r\n".to_vec();
        stream.extend_from_slice(MARKER);
        stream.extend_from_slice(b":BEGIN");
        stream.extend_from_slice(separator);
        stream.extend_from_slice(b"BODY\r\n\r\n");
        stream.extend_from_slice(MARKER);
        stream.extend_from_slice(b":0\r\n");
        let splits: Vec<usize> = (1..stream.len()).collect();

        let view = scan(&stream, &splits)
            .unwrap_or_else(|| unreachable!("every separator split must complete"));

        assert_eq!(view.exit_status, Some(0));
        assert_eq!(view.output, "BODY\n");
    }
}

#[test]
fn a_run_that_never_answered_is_reported_as_no_shell() {
    let mut scanner = scanner();
    assert!(scanner.push(b"some editor drew a screen").is_none());

    let view = scanner.unfinished(RunOutcome::Deadline, "%0".to_owned());

    assert_eq!(view.outcome, RunOutcome::NoShell);
    assert!(view.exit_status.is_none());
}

#[test]
fn a_run_still_going_at_its_deadline_keeps_that_outcome() {
    let mut scanner = scanner();
    let stream = [MARKER, b":BEGIN\nworking".as_slice()].concat();
    assert!(scanner.push(&stream).is_none());

    let view = scanner.unfinished(RunOutcome::Deadline, "%0".to_owned());

    assert_eq!(
        view.outcome,
        RunOutcome::Deadline,
        "the shell answered, so the command is merely slow"
    );
}

#[test]
fn a_half_arrived_status_is_not_reported() {
    let mut scanner = scanner();
    let stream = [
        MARKER,
        b":BEGIN\nout\n".as_slice(),
        MARKER,
        b":12".as_slice(),
    ]
    .concat();

    assert!(
        scanner.push(&stream).is_none(),
        "reading 1 from a status of 12 would be worse than waiting"
    );
}

#[test]
fn marker_lookalikes_are_command_output() {
    let mut scanner = scanner();
    let mut stream = Vec::new();
    stream.extend_from_slice(MARKER);
    stream.extend_from_slice(b":BEGIN\n");
    stream.push(b'x');
    stream.extend_from_slice(MARKER);
    stream.extend_from_slice(b":0\n");
    stream.extend_from_slice(MARKER);
    stream.extend_from_slice(b":BEGIN suffix\n");
    stream.extend_from_slice(MARKER);
    stream.extend_from_slice(b":256\n");
    stream.extend_from_slice(MARKER);
    stream.extend_from_slice(b":not-a-status\n\n");
    stream.extend_from_slice(MARKER);
    stream.extend_from_slice(b":00\nafter-lookalike\n");
    stream.extend_from_slice(MARKER);
    stream.extend_from_slice(b":0\n");

    let view = scanner
        .push(&stream)
        .unwrap_or_else(|| unreachable!("the exact closing line completes"));

    assert!(view.output.starts_with("x__LIBTMUX_MCP_DONE_"));
    assert!(view.output.contains(":BEGIN suffix\n"));
    assert!(view.output.contains(":256\n"));
    assert!(view.output.contains(":00\n"));
    assert_eq!(view.exit_status, Some(0));
}

/// No frame line may exceed a terminal's canonical input limit.
///
/// The frame is typed into a pane, not piped, so every line passes through a
/// terminal in canonical mode. `MAX_CANON` bounds one such line: 1024 bytes
/// on macOS and the BSDs against 4096 on Linux. A longer line is never
/// delivered, so the shell waits at its continuation prompt and the run
/// reports `no_shell` until it times out -- with no error anywhere, because
/// nothing failed, the input simply never arrived.
///
/// Joined with `; `, the bash trap setup reached 1715 bytes and zsh 1115.
/// Both worked on Linux and neither could work on macOS.
#[test]
fn no_frame_line_exceeds_a_terminal_input_limit() {
    // macOS and the BSDs, which is the tightest limit this runs against.
    const MAX_CANON: usize = 1024;

    for shell in [b"sh".as_slice(), b"bash", b"zsh"] {
        for suppress_history in [false, true] {
            let payload = render_payload(
                OsStr::new("/opt/homebrew/bin/tmux"),
                Path::new("/tmp/libtmux-rs-test/agent-preserves-raw.sock"),
                "63bdf5760a1f845190b25ac835320917",
                shell,
                OsStr::new("printf body # trailing comment"),
                suppress_history,
            );
            let longest = payload
                .as_bytes()
                .split(|byte| *byte == b'\n')
                .map(<[u8]>::len)
                .max()
                .unwrap_or(0);

            assert!(
                longest < MAX_CANON,
                "{} frame has a {longest}-byte line, over the {MAX_CANON}-byte \
                 terminal limit; it cannot be typed into a pane on macOS",
                String::from_utf8_lossy(shell)
            );
        }
    }
}
