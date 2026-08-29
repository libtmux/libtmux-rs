use super::*;

#[test]
fn finding_a_needle_reports_where_it_starts() {
    assert_eq!(find(b"abcdef", b"cd"), Some(2));
    assert_eq!(find(b"abcdef", b"xy"), None);
    assert_eq!(find(b"ab", b"abcdef"), None);
    assert_eq!(find(b"abc", b""), None);
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

/// Feed a scanner one run's stream, split at the given byte offsets.
fn scan(stream: &[u8], splits: &[usize]) -> Option<RunView> {
    let mut scanner = Scanner::new(b"\x1b_Ns\x1b\\".to_vec(), b"\x1b_Ne;".to_vec());
    let mut at = 0;
    let mut finished = None;
    for &next in splits.iter().chain(std::iter::once(&stream.len())) {
        let chunk = &stream[at..next];
        at = next;
        finished = finished.or_else(|| scanner.push(chunk));
    }
    finished
}

fn one_run() -> Vec<u8> {
    let mut stream = Vec::new();
    stream.extend_from_slice(br"printf '\033_Ns\033\\'; ( echo hi ); ");
    stream.extend_from_slice(b"\r\n\x1b_Ns\x1b\\hi\r\n\x1b_Ne;42\x1b\\");
    stream
}

#[test]
fn a_run_arriving_whole_is_read() {
    let view = scan(&one_run(), &[]).unwrap_or_else(|| unreachable!("the run completed"));

    assert_eq!(view.exit_status, Some(42));
    assert_eq!(view.output, "hi\n");
}

#[test]
fn scanner_publishes_state_at_a_trimmed_body_start() {
    let opened = b"\x1b_Ns\x1b\\".to_vec();
    let mut scanner = Scanner::new(opened.clone(), b"\x1b_Ne;".to_vec());
    let mut body = b"\x1b[31mred".to_vec();
    body.resize(OUTPUT_LIMIT + 4, b'x');
    let mut stream = opened;
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
}

#[test]
fn a_close_waiting_for_status_does_not_suspend_trimming() {
    let opened = b"\x1b_Ns\x1b\\".to_vec();
    let closed = b"\x1b_Ne;".to_vec();
    let mut scanner = Scanner::new(opened.clone(), closed.clone());
    let mut chunk = opened;
    chunk.resize(OUTPUT_LIMIT + 32, b'x');
    chunk.extend_from_slice(&closed);

    assert!(scanner.push(&chunk).is_none());

    assert_eq!(scanner.retained().len(), OUTPUT_LIMIT);
}

#[test]
fn completed_output_resumes_at_the_trim_checkpoint() {
    let opened = b"\x1b_Ns\x1b\\".to_vec();
    let closed = b"\x1b_Ne;".to_vec();
    let ending = [closed.as_slice(), b"0\x1b\\"].concat();
    let mut body = b"\x1b[31mred".to_vec();
    body.resize(OUTPUT_LIMIT + 4 - ending.len(), b'x');
    let mut stream = opened.clone();
    stream.extend_from_slice(&body);
    stream.extend_from_slice(&ending);
    let mut scanner = Scanner::new(opened, closed);

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
fn a_run_split_between_its_sentinel_and_its_status_is_still_read() {
    // tmux decides where a chunk ends. Splitting immediately after the
    // closing sentinel leaves the status digits for a later chunk, by
    // which time the sentinel is behind everything newly scanned.
    let stream = one_run();
    let after_sentinel = stream.len() - "42\x1b\\".len();

    let view = scan(&stream, &[after_sentinel])
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
fn a_run_that_never_answered_is_reported_as_no_shell() {
    let mut scanner = Scanner::new(b"\x1b_Ns\x1b\\".to_vec(), b"\x1b_Ne;".to_vec());
    assert!(scanner.push(b"some editor drew a screen").is_none());

    let view = scanner.unfinished(RunOutcome::Deadline, "%0".to_owned());

    assert_eq!(view.outcome, RunOutcome::NoShell);
    assert!(view.exit_status.is_none());
}

#[test]
fn a_run_still_going_at_its_deadline_keeps_that_outcome() {
    let mut scanner = Scanner::new(b"\x1b_Ns\x1b\\".to_vec(), b"\x1b_Ne;".to_vec());
    assert!(scanner.push(b"\x1b_Ns\x1b\\working").is_none());

    let view = scanner.unfinished(RunOutcome::Deadline, "%0".to_owned());

    assert_eq!(
        view.outcome,
        RunOutcome::Deadline,
        "the shell answered, so the command is merely slow"
    );
}

#[test]
fn a_status_is_read_from_between_the_sentinels() {
    let opened = b"\x1b_1s\x1b\\";
    let closed = b"\x1b_1e;";
    let mut stream = Vec::new();
    stream.extend_from_slice(b"echo hi\r\n");
    stream.extend_from_slice(opened);
    stream.extend_from_slice(b"hi\r\n");
    stream.extend_from_slice(closed);
    stream.extend_from_slice(b"7\x1b\\");

    let at = find(&stream, closed).unwrap_or_else(|| unreachable!("the sentinel is present"));
    let view =
        finished(&stream, at, opened, closed).unwrap_or_else(|| unreachable!("the block is whole"));

    assert_eq!(view.exit_status, Some(7));
    assert_eq!(view.output, "hi\n");
}

#[test]
fn a_half_arrived_status_is_not_reported() {
    let opened = b"\x1b_1s\x1b\\";
    let closed = b"\x1b_1e;";
    let mut stream = Vec::new();
    stream.extend_from_slice(opened);
    stream.extend_from_slice(b"out");
    stream.extend_from_slice(closed);
    stream.extend_from_slice(b"12");

    let at = find(&stream, closed).unwrap_or_else(|| unreachable!("the sentinel is present"));

    assert!(
        finished(&stream, at, opened, closed).is_none(),
        "reading 1 from a status of 12 would be worse than waiting"
    );
}

#[test]
fn the_echoed_command_is_not_mistaken_for_a_sentinel() {
    let opened = b"\x1b_ab1s\x1b\\";
    let closed = b"\x1b_ab1e;";
    let mut stream = Vec::new();
    // What a shell echoes: the source text, where the escape is four
    // ordinary characters.
    stream.extend_from_slice(br"printf '\033_ab1s\033\\'; ( echo hi ); ");
    stream.extend_from_slice(br"printf '\033_ab1e;%d\033\\' $s");
    stream.extend_from_slice(b"\r\n");
    stream.extend_from_slice(opened);
    stream.extend_from_slice(b"hi\r\n");
    stream.extend_from_slice(closed);
    stream.extend_from_slice(b"0\x1b\\");

    let at = find(&stream, closed).unwrap_or_else(|| unreachable!("the sentinel is present"));
    let view =
        finished(&stream, at, opened, closed).unwrap_or_else(|| unreachable!("the block is whole"));

    assert_eq!(
        view.output, "hi\n",
        "the echo sits before the opening sentinel and is discarded whole"
    );
    assert_eq!(view.exit_status, Some(0));
}
