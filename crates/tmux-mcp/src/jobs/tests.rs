use super::*;

fn job(index: usize, state: JobState, last_read: Instant) -> Job {
    let progress = Arc::new(Mutex::new(Progress {
        state,
        exit_status: None,
        stream: Vec::new(),
        body: None,
        dropped: 0,
        checkpoint: TextFilter::new(),
        bytes: 0,
        truncated: false,
        terminal: None,
    }));
    Job {
        pane: format!("%{index}"),
        command: "sleep 60".to_owned(),
        started: last_read,
        progress,
        finished: Arc::new(Notify::new()),
        reader: tokio::spawn(std::future::pending()),
        last_read,
    }
}

async fn wait_for_prompt(pane: &Pane) {
    for _ in 0..600 {
        let lines = pane.capture().await.expect("pane captures");
        if lines
            .iter()
            .any(|line| matches!(line.as_bytes().last(), Some(b'$' | b'#')))
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    panic!("the pane never drew a prompt");
}

async fn session_fixture(name: &str) -> (libtmux::test::TestServer, libtmux::Session) {
    let guard = libtmux::test::TestServer::builder()
        .start()
        .await
        .expect("tmux starts");
    let session = guard
        .server()
        .new_session(name)
        .await
        .expect("session starts");
    (guard, session)
}

async fn block_line_reply(session: &libtmux::Session, sent: &str, release: &str) {
    session
        .set_hook(
            "after-send-keys",
            format!(
                "if-shell -F '#{{==:#{{hook_flag_l}},1}}' \
                 'wait-for -S {sent}; wait-for {release}'"
            ),
        )
        .await
        .expect("the start gate is installed");
}

fn progress(output: &[u8], dropped: u64) -> Progress {
    Progress {
        state: JobState::Running,
        exit_status: None,
        stream: output.to_vec(),
        body: Some(0..output.len()),
        dropped,
        checkpoint: TextFilter::new(),
        bytes: output.len(),
        truncated: dropped > 0,
        terminal: None,
    }
}

#[test]
fn a_cursor_reads_only_what_is_new() {
    let held = progress(b"hello world", 0);

    assert_eq!(held.read_from(0), (&b"hello world"[..], 11, false));
    assert_eq!(held.read_from(6), (&b"world"[..], 11, false));
    assert_eq!(held.read_from(11), (&b""[..], 11, false));
}

#[test]
fn a_cursor_behind_what_was_trimmed_says_so() {
    // The first 100 bytes were dropped, so `output` starts at offset 100.
    let held = progress(b"tail", 100);

    let (bytes, end, truncated) = held.read_from(0);
    assert!(truncated, "the bytes at offset 0 are gone");
    assert_eq!(bytes, b"tail");
    assert_eq!(end, 104);

    let (bytes, _, truncated) = held.read_from(100);
    assert!(!truncated);
    assert_eq!(bytes, b"tail");
}

#[test]
fn a_cursor_past_the_end_yields_nothing_rather_than_panicking() {
    let held = progress(b"hello", 0);

    assert_eq!(held.read_from(99), (&b""[..], 5, false));
}

#[test]
fn reusing_a_job_cursor_resumes_its_filter_state() {
    let mut checkpoint = TextFilter::new();
    checkpoint.advance(b"\x1b[31");
    let mut held = progress(b"", 0);
    held.replace(exec::RunProgress {
        stream: b"mred",
        body: Some(0..4),
        body_dropped: 4,
        body_checkpoint: &checkpoint,
        bytes: 8,
        truncated: true,
    });
    let cursor = 0;

    let (text, _, truncated) = held.text_from(cursor);
    assert_eq!(text, "red");
    assert!(truncated);

    held.stream.extend_from_slice(b"!");
    held.body.as_mut().expect("the body is known").end += 1;

    assert_eq!(held.text_from(cursor).0, "red!");
}

#[test]
fn a_terminal_result_wins_a_simultaneous_foreground_stop() {
    let mut held = progress(b"finished\r\n", 0);
    held.state = JobState::Finished;
    held.exit_status = Some(0);
    held.terminal = Some(RunView {
        pane: "%0".to_owned(),
        outcome: RunOutcome::Completed,
        exit_status: Some(0),
        output: "finished\n".to_owned(),
        bytes: 10,
        truncated: false,
        job: None,
    });

    let (view, retain) = held
        .foreground_view("%0".to_owned(), Some(RunOutcome::Cancelled))
        .expect("the terminal result is available");

    assert_eq!(view.outcome, RunOutcome::Completed);
    assert_eq!(view.exit_status, Some(0));
    assert!(!retain, "terminal work needs no recovery job");
}

#[test]
fn job_ids_are_not_reused_after_counter_exhaustion() {
    let mut table = JobTable::new(1);
    table.next_id = Some(u64::MAX);
    let owner = InstanceIdentity::fixed(1)
        .get()
        .expect("the fixed identity is available");

    table.reserve(owner).expect("the last id is available");
    assert!(
        matches!(table.reserve(owner), Err(StartError::IdSpaceExhausted)),
        "an exhausted counter wrapped to a previously issued id",
    );
}

#[tokio::test]
async fn a_full_running_table_refuses_before_sending() {
    let guard = libtmux::test::TestServer::builder()
        .start()
        .await
        .expect("tmux starts");
    let session = guard
        .server()
        .new_session("job-capacity-red")
        .await
        .expect("session starts");
    let pane = session.panes().await.expect("panes list").remove(0);
    wait_for_prompt(&pane).await;
    let jobs = Jobs::with_limit(1);
    let reservation = jobs.reserve().expect("the first slot is free");
    let first = reservation.id().to_owned();
    reservation.commit(job(0, JobState::Running, Instant::now()));

    let marker = "capacity-rejection-must-not-reach-the-pane";
    let started = jobs.start(&pane, &format!("echo {marker}"), false).await;

    assert!(
        matches!(started, Err(StartError::AtCapacity { limit: 1 })),
        "a full running table accepted a job: {started:?}",
    );
    assert!(jobs.holds(&first), "the first job remains");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let reached_pane = pane
        .capture()
        .await
        .expect("pane captures")
        .iter()
        .any(|line| line.to_string_lossy().contains(marker));
    assert!(!reached_pane, "the rejected command was sent to the pane");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_failed_start_releases_its_reservation() {
    let guard = libtmux::test::TestServer::builder()
        .start()
        .await
        .expect("tmux starts");
    let session = guard
        .server()
        .new_session("job-capacity-failure")
        .await
        .expect("session starts");
    let pane = session.panes().await.expect("panes list").remove(0);
    guard.shutdown().await.expect("tmux fixture shuts down");
    let jobs = Jobs::with_limit(1);

    let started = jobs.start(&pane, "true", false).await;

    assert!(matches!(started, Err(StartError::Tmux(_))));
    assert!(
        jobs.reserve().is_ok(),
        "a failed start retained its pending slot",
    );
}

#[tokio::test]
async fn invalid_line_is_not_an_unknown_dispatch() {
    let (guard, session) = session_fixture("job-start-invalid-input").await;
    let pane = session.panes().await.expect("panes list").remove(0);
    wait_for_prompt(&pane).await;
    let jobs = Jobs::with_limit(1);

    let started = jobs.start(&pane, "printf untouched\0", false).await;

    assert!(
        matches!(
            started,
            Err(StartError::Tmux(Error::InvalidCommandInput { .. }))
        ),
        "input rejected before dispatch was reported as uncertain: {started:?}",
    );
    assert!(jobs.list().is_empty(), "an untouched start was retained");
    assert!(
        !pane
            .capture()
            .await
            .expect("pane captures")
            .iter()
            .any(|line| line.to_string_lossy().contains("untouched")),
        "invalid input reached the pane",
    );
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn cancelling_after_send_keeps_the_start_visible() {
    let (guard, session) = session_fixture("job-start-cancel").await;
    let pane = session.panes().await.expect("panes list").remove(0);
    wait_for_prompt(&pane).await;

    let jobs = Arc::new(Jobs::with_limit(1));
    let sent = "job-start-cancel-sent";
    let release = "job-start-cancel-release";
    block_line_reply(&session, sent, release).await;
    let marker = "job-start-crossed-tmux";
    let starting = tokio::spawn({
        let jobs = Arc::clone(&jobs);
        let pane = pane.clone();
        async move {
            jobs.start(&pane, &format!("printf '{marker}\\n'; sleep 60"), false)
                .await
        }
    });

    assert_eq!(
        guard
            .server()
            .wait_for_channel(sent, std::time::Duration::from_secs(5))
            .await
            .expect("the gate channel can be read"),
        libtmux::ChannelWait::Signalled,
        "the line hook did not run",
    );
    assert_eq!(
        pane.wait_for_text(marker, std::time::Duration::from_secs(5))
            .await
            .expect("the pane can be read"),
        libtmux::PaneWait::Arrived,
        "the command did not begin while its reply was blocked",
    );

    starting.abort();
    assert!(
        starting
            .await
            .expect_err("the caller's start future was cancelled")
            .is_cancelled(),
    );
    let visible = jobs.list();
    guard
        .server()
        .signal_channel(release)
        .await
        .expect("the blocked tmux command is released");

    let owned = visible
        .into_iter()
        .next()
        .expect("the cancelled start remains visible");
    assert_eq!(owned.pane, pane.id().to_string());
    assert_eq!(owned.state, JobState::Starting);
    assert!(jobs.holds(&owned.job));
    libtmux::test::retry_until(std::time::Duration::from_secs(5), async || {
        jobs.read(&owned.job, None)
            .is_some_and(|progress| progress.output.contains(marker))
    })
    .await
    .expect("the owned watcher retains output written before publication");
    assert_eq!(jobs.forget(&owned.job), Some(pane.id().to_string()));
    assert!(jobs.read(&owned.job, None).is_none());
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn forgetting_a_start_does_not_claim_that_it_was_retained() {
    let (guard, session) = session_fixture("job-start-forget").await;
    let pane = session.panes().await.expect("panes list").remove(0);
    wait_for_prompt(&pane).await;

    let jobs = Arc::new(Jobs::with_limit(1));
    let sent = "job-start-forget-sent";
    let release = "job-start-forget-release";
    block_line_reply(&session, sent, release).await;
    let starting = tokio::spawn({
        let jobs = Arc::clone(&jobs);
        let pane = pane.clone();
        async move { jobs.start(&pane, "sleep 60", false).await }
    });

    assert_eq!(
        guard
            .server()
            .wait_for_channel(sent, std::time::Duration::from_secs(5))
            .await
            .expect("the gate channel can be read"),
        libtmux::ChannelWait::Signalled,
        "the line hook did not run",
    );
    let owned = jobs
        .list()
        .into_iter()
        .next()
        .expect("the starting job is visible");
    assert_eq!(jobs.forget(&owned.job), Some(pane.id().to_string()));
    guard
        .server()
        .signal_channel(release)
        .await
        .expect("the blocked tmux command is released");

    assert!(
        matches!(
            starting.await.expect("the start task remains healthy"),
            Err(StartError::WorkerStopped)
        ),
        "a removed job was reported as retained",
    );
    assert!(jobs.read(&owned.job, None).is_none());
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_timed_out_send_retains_an_inspectable_job() {
    let (guard, session) = session_fixture("job-start-timeout").await;
    let short = libtmux::Server::builder()
        .socket_path(guard.server().socket_path())
        .config_file(guard.server().config_file().expect("the fixture config"))
        .tmux_executable(guard.server().tmux_executable())
        .default_timeout(std::time::Duration::from_secs(2))
        .build()
        .expect("a short-deadline handle builds");
    let pane = short
        .session("job-start-timeout")
        .await
        .expect("the session can be listed")
        .expect("the session exists")
        .panes()
        .await
        .expect("panes list")
        .remove(0);
    wait_for_prompt(&pane).await;

    let sent = "job-start-timeout-sent";
    let release = "job-start-timeout-release";
    block_line_reply(&session, sent, release).await;
    let marker = "job-start-timeout-crossed-tmux";
    let jobs = Jobs::with_limit(1);
    let started = jobs
        .start(&pane, &format!("printf '{marker}\\n'; sleep 60"), false)
        .await;

    let Err(StartError::DispatchUnknown {
        job: retained_id,
        cause: DispatchFailure::Tmux(error),
    }) = started
    else {
        panic!("the blocked reply is a retained dispatch error: {started:?}");
    };
    assert_eq!(error.kind(), libtmux::ErrorKind::Timeout);
    assert_eq!(
        pane.wait_for_text(marker, std::time::Duration::from_secs(5))
            .await
            .expect("the pane can be read"),
        libtmux::PaneWait::Arrived,
        "tmux began the command before its client timed out",
    );
    let visible = jobs.list();
    guard
        .server()
        .signal_channel(release)
        .await
        .expect("the timed-out hook is released");

    let owned = visible
        .into_iter()
        .next()
        .expect("an uncertain dispatch remains visible");
    assert_eq!(owned.job, retained_id);
    assert_eq!(owned.state, JobState::DispatchUnknown);
    libtmux::test::retry_until(std::time::Duration::from_secs(5), async || {
        jobs.read(&owned.job, None)
            .is_some_and(|progress| progress.output.contains(marker))
    })
    .await
    .expect("the watcher retains output after dispatch times out");
    assert!(jobs.holds(&owned.job));
    pane.send_key_names(["C-c"])
        .await
        .expect("an uncertain job can be interrupted");
    assert_eq!(jobs.forget(&owned.job), Some(pane.id().to_string()));
    assert!(jobs.read(&owned.job, None).is_none());
    short
        .shutdown()
        .await
        .expect("the short executor shuts down");
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn a_finished_lru_is_evicted_before_a_running_job() {
    let jobs = Jobs::with_limit(3);
    let now = Instant::now();
    let entries = [
        (
            JobState::Running,
            now.checked_sub(std::time::Duration::from_secs(3))
                .expect("the process has run for three seconds"),
        ),
        (
            JobState::Finished,
            now.checked_sub(std::time::Duration::from_secs(2))
                .expect("the process has run for two seconds"),
        ),
        (
            JobState::Finished,
            now.checked_sub(std::time::Duration::from_secs(1))
                .expect("the process has run for one second"),
        ),
    ];
    let mut ids = Vec::new();
    for (index, (state, last_read)) in entries.into_iter().enumerate() {
        let reservation = jobs.reserve().expect("a slot is free");
        ids.push(reservation.id().to_owned());
        reservation.commit(job(index, state, last_read));
    }

    let next = jobs.reserve().expect("a finished slot makes room");

    assert!(jobs.holds(&ids[0]), "the running job remains");
    assert!(
        !jobs.holds(&ids[1]),
        "the least recently read finished job is evicted",
    );
    assert!(jobs.holds(&ids[2]), "the newer finished job remains");
    drop(next);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn forgetting_a_job_wakes_a_registered_status_waiter() {
    let jobs = Arc::new(Jobs::with_limit(1));
    let id = "job-wait-race".to_owned();
    let now = Instant::now();
    hold(&jobs.inner)
        .slots
        .insert(id.clone(), JobSlot::Ready(job(0, JobState::Running, now)));
    let ready = Arc::new(std::sync::Barrier::new(2));
    let release = Arc::new(std::sync::Barrier::new(2));
    let waiting = tokio::spawn({
        let jobs = Arc::clone(&jobs);
        let id = id.clone();
        let ready = Arc::clone(&ready);
        let release = Arc::clone(&release);
        async move {
            jobs.wait_with(&id, std::time::Duration::from_secs(20), || {
                ready.wait();
                release.wait();
            })
            .await
        }
    });

    ready.wait();
    assert_eq!(jobs.forget(&id).as_deref(), Some("%0"));
    assert!(!jobs.holds(&id), "the active owner is removed");
    release.wait();

    let woke = tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
        .await
        .expect("forgetting the owner wakes the registered waiter")
        .expect("the wait task stays healthy");
    assert!(woke, "the owner-drop notification reached the waiter");
}

#[tokio::test]
async fn forgetting_a_ready_job_returns_its_pane_and_aborts_its_reader() {
    let jobs = Jobs::with_limit(1);
    let id = "job-forget-ready".to_owned();
    let now = Instant::now();
    let (started, started_by_reader) = oneshot::channel();
    let reader = tokio::spawn(async move {
        let _ = started.send(());
        std::future::pending::<()>().await;
    });
    let abort = reader.abort_handle();
    started_by_reader
        .await
        .expect("the reader starts before the table owns it");
    hold(&jobs.inner).slots.insert(
        id.clone(),
        JobSlot::Ready(Job {
            pane: "%7".to_owned(),
            command: "sleep 60".to_owned(),
            started: now,
            progress: Arc::new(Mutex::new(Progress {
                state: JobState::Running,
                exit_status: None,
                stream: Vec::new(),
                body: None,
                dropped: 0,
                checkpoint: TextFilter::new(),
                bytes: 0,
                truncated: false,
                terminal: None,
            })),
            finished: Arc::new(Notify::new()),
            reader,
            last_read: now,
        }),
    );

    assert_eq!(jobs.forget(&id).as_deref(), Some("%7"));
    assert!(!jobs.holds(&id), "forgetting removes the published job");
    assert!(jobs.read(&id, None).is_none());
    assert!(jobs.list().is_empty());
    libtmux::test::retry_until(std::time::Duration::from_secs(2), async || {
        abort.is_finished()
    })
    .await
    .expect("forgetting an active job aborts its reader");
}

#[test]
fn forgetting_an_absent_or_unpublished_job_returns_none() {
    let jobs = Jobs::with_limit(1);

    assert_eq!(jobs.forget("job-missing"), None);
    let reservation = jobs.reserve().expect("the slot is available");
    assert_eq!(jobs.forget(reservation.id()), None);
    assert!(
        matches!(
            hold(&jobs.inner).slots.get(reservation.id()),
            Some(JobSlot::Pending)
        ),
        "forgetting an unpublished job leaves its reservation intact",
    );
}

#[tokio::test]
async fn dropping_a_finished_job_does_not_abort_its_reader() {
    let (release, released) = oneshot::channel::<()>();
    let (complete, completed) = oneshot::channel::<()>();
    let reader = tokio::spawn(async move {
        if released.await.is_ok() {
            let _ = complete.send(());
        }
    });
    let now = Instant::now();
    let finished = Job {
        pane: "%0".to_owned(),
        command: "true".to_owned(),
        started: now,
        progress: Arc::new(Mutex::new(Progress {
            state: JobState::Finished,
            exit_status: Some(0),
            stream: Vec::new(),
            body: Some(0..0),
            dropped: 0,
            checkpoint: TextFilter::new(),
            bytes: 0,
            truncated: false,
            terminal: None,
        })),
        finished: Arc::new(Notify::new()),
        reader,
        last_read: now,
    };

    drop(finished);
    let _ = release.send(());

    assert!(
        completed.await.is_ok(),
        "a terminal reader was aborted before it could notify waiters",
    );
}

#[tokio::test]
async fn pending_reservations_cannot_over_admit_and_release_on_drop() {
    let jobs = Jobs::with_limit(2);
    let ready = std::sync::Barrier::new(5);
    let attempted = std::sync::Barrier::new(5);
    let release = std::sync::Barrier::new(5);

    let admitted = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    ready.wait();
                    let held = jobs.reserve();
                    let admitted = held.is_ok();
                    attempted.wait();
                    release.wait();
                    drop(held);
                    admitted
                })
            })
            .collect();
        ready.wait();
        attempted.wait();
        release.wait();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("thread joins"))
            .filter(|admitted| *admitted)
            .count()
    });

    assert_eq!(admitted, 2, "only the table's limit is admitted");
    {
        let cancelled = async {
            let _held = jobs.reserve().expect("a slot is free");
            std::future::pending::<()>().await;
        };
        tokio::pin!(cancelled);
        tokio::select! {
            biased;
            () = &mut cancelled => panic!("the pending start completed"),
            () = async {} => {}
        }
    }
    assert!(
        jobs.reserve().is_ok(),
        "cancelling an unfinished reservation releases its slot",
    );
}
