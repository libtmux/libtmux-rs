#![allow(clippy::err_expect, clippy::expect_used, clippy::ok_expect)]
use super::*;
use crate::formats::{FormatPlan, ListProfile};
use crate::test::TestServer;
use crate::{Command, PaneId, ReleaseSuffix, ReleaseVersion, SessionId, TmuxVersion, WindowId};

#[cfg(feature = "test-support")]
fn projection_test_executable() -> std::ffi::OsString {
    std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| std::ffi::OsString::from("tmux"))
}

#[cfg(feature = "test-support")]
async fn projection_test_server() -> (TestServer, std::ffi::OsString, Option<std::ffi::OsString>) {
    let explicit = std::env::var_os("LIBTMUX_TEST_TMUX");
    let executable = projection_test_executable();
    let guard = TestServer::builder()
        .tmux_executable(executable.clone())
        .start()
        .await
        .expect("the explicitly selected tmux starts");
    assert!(
        guard.server().tmux_executable() == executable.as_os_str(),
        "TestServer retains the selected tmux executable",
    );
    (guard, executable, explicit)
}

#[cfg(feature = "test-support")]
async fn projection_run(server: &crate::Server, command: Command) {
    let result = server
        .cmd(command)
        .await
        .expect("tmux setup command executes");
    assert!(result.success(), "tmux setup command succeeds");
    assert!(result.stderr().is_empty(), "tmux setup stderr is empty");
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn real_tmux_session_listing_hydrates_ordered_named_snapshots() {
    let (guard, _executable, _explicit) = projection_test_server().await;
    let server = guard.server();

    for name in ["alpha", "beta"] {
        projection_run(
            server,
            Command::new("new-session")
                .arg("-d")
                .arg("-s")
                .arg(name)
                .arg("sleep 120"),
        )
        .await;
    }

    let version = server
        .capabilities()
        .await
        .expect("tmux capabilities are detected")
        .tmux_version();
    let plan = FormatPlan::for_profile(ListProfile::Sessions, version);
    let result = server
        .cmd(Command::new("list-sessions").arg("-F").arg(plan.template()))
        .await
        .expect("the session listing executes");
    assert!(result.success(), "the session listing succeeds");

    let sessions = hydrate_session_infos_from_stdout(&plan, result.stdout())
        .ok()
        .expect("live session rows hydrate");

    let names = sessions
        .iter()
        .map(|session| session.session_name().as_bytes().to_vec())
        .collect::<Vec<_>>();
    assert_eq!(names, [b"alpha".to_vec(), b"beta".to_vec()]);

    // The baseline identity is unconditional, so it needs no evidence.
    assert!(
        sessions
            .iter()
            .all(|session| AsRef::<str>::as_ref(session.session_id()).starts_with('$')),
        "every hydrated session carries its tmux identity",
    );

    // tmux emits nothing for a session it has never attached. Treating
    // that as a required field made every `new-session -d` listing fail
    // to hydrate, which is the ordinary scripted and CI case.
    assert!(
        sessions
            .iter()
            .all(|session| session.session_last_attached() == Availability::Absent),
        "a never-attached session reports an absent last-attach time",
    );

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[cfg(feature = "test-support")]
async fn projection_stdout(server: &crate::Server, command: Command) -> String {
    let result = server
        .cmd(command)
        .await
        .expect("tmux scalar query executes");
    assert!(result.success(), "tmux scalar query succeeds");
    assert!(result.stderr().is_empty(), "tmux scalar stderr is empty");
    result
        .stdout_utf8()
        .expect("tmux scalar output is strict UTF-8")
        .to_owned()
}

#[cfg(feature = "test-support")]
fn projection_uses_post_3_6_semantics(version: &TmuxVersion) -> bool {
    match version.release() {
        Some(release) => *release >= ReleaseVersion::new(3, 6, ReleaseSuffix::FINAL),
        None => true,
    }
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated fixture proves the repeated-winlink scalar shape end to end"
)]
async fn real_tmux_compat_projection_repeated_winlinks_preserve_rows_and_raw_aggregates() {
    let (guard, executable, explicit) = projection_test_server().await;
    let server = guard.server();
    if let Some(expected) = explicit {
        assert!(
            executable == expected,
            "the selected executable matches LIBTMUX_TEST_TMUX",
        );
    }
    assert!(
        server.tmux_executable() == executable.as_os_str(),
        "the Server retains the selected tmux executable",
    );

    projection_run(
        server,
        Command::new("new-session")
            .arg("-d")
            .arg("-s")
            .arg("dup")
            .arg("-n")
            .arg("repeated")
            .arg("sleep 120"),
    )
    .await;
    projection_run(
        server,
        Command::new("move-window")
            .arg("-s")
            .arg("dup:0")
            .arg("-t")
            .arg("dup:1"),
    )
    .await;
    projection_run(
        server,
        Command::new("new-window")
            .arg("-d")
            .arg("-t")
            .arg("dup:2")
            .arg("-n")
            .arg("unrelated")
            .arg("sleep 120"),
    )
    .await;
    projection_run(
        server,
        Command::new("link-window")
            .arg("-s")
            .arg("dup:1")
            .arg("-t")
            .arg("dup:5"),
    )
    .await;

    let repeated_session_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("dup")
            .arg("#{session_id}"),
    )
    .await
    .trim()
    .parse::<SessionId>()
    .expect("the repeated-link Session ID is valid");
    let repeated_window_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("dup:1")
            .arg("#{window_id}"),
    )
    .await
    .trim()
    .parse::<WindowId>()
    .expect("the repeated Window ID is valid");
    let repeated_pane_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("dup:1.0")
            .arg("#{pane_id}"),
    )
    .await
    .trim()
    .parse::<PaneId>()
    .expect("the repeated Pane ID is valid");
    let version = server
        .capabilities()
        .await
        .expect("tmux capabilities are detected")
        .tmux_version();
    let expected_count = if projection_uses_post_3_6_semantics(version) {
        1
    } else {
        2
    };
    let window_plan = window_projection_plan(version)
        .ok()
        .expect("the detected Window projection plan is valid");
    let window_result = server
        .cmd(
            Command::new("list-windows")
                .arg("-t")
                .arg("dup")
                .arg("-F")
                .arg(window_plan.template()),
        )
        .await
        .expect("the Window projection listing executes");
    assert!(
        window_result.success(),
        "the Window projection listing succeeds"
    );
    assert!(
        window_result.stderr().is_empty(),
        "the Window projection stderr is empty"
    );
    let windows = hydrate_window_projections_from_stdout(
        server.identity(),
        &window_plan,
        window_result.stdout(),
    )
    .ok()
    .expect("the Window projection stdout parses and hydrates");
    let repeated_windows: Vec<_> = windows
        .iter()
        .filter(|projection| {
            projection.window().window_id().as_ref() == repeated_window_id.as_ref()
        })
        .collect();
    assert_eq!(repeated_windows.len(), 2);
    assert_eq!(
        repeated_windows
            .iter()
            .map(|projection| projection.link().identity().window_index())
            .collect::<Vec<_>>(),
        [1, 5],
    );
    for projection in &repeated_windows {
        assert_eq!(
            projection.link().identity().server_identity(),
            server.identity()
        );
        assert_eq!(
            projection.link().identity().session_id(),
            &repeated_session_id
        );
        assert_eq!(
            projection.link().identity().window_id(),
            &repeated_window_id
        );
        assert_eq!(projection.window().window_id(), &repeated_window_id);
        assert_eq!(projection.window_linked_sessions(), expected_count);
        assert_eq!(
            projection.window_linked_sessions_list().as_bytes(),
            b"dup,dup"
        );
    }

    let pane_plan = pane_projection_plan(version)
        .ok()
        .expect("the detected Pane projection plan is valid");
    let pane_result = server
        .cmd(
            Command::new("list-panes")
                .arg("-a")
                .arg("-F")
                .arg(pane_plan.template()),
        )
        .await
        .expect("the Pane projection listing executes");
    assert!(
        pane_result.success(),
        "the Pane projection listing succeeds"
    );
    assert!(
        pane_result.stderr().is_empty(),
        "the Pane projection stderr is empty"
    );
    let panes =
        hydrate_pane_projections_from_stdout(server.identity(), &pane_plan, pane_result.stdout())
            .ok()
            .expect("the Pane projection stdout parses and hydrates");
    let repeated_panes: Vec<_> = panes
        .iter()
        .filter(|projection| projection.pane().pane_id().as_ref() == repeated_pane_id.as_ref())
        .collect();
    assert_eq!(repeated_panes.len(), 2);
    assert_eq!(
        repeated_panes
            .iter()
            .map(|projection| projection.link_identity().window_index())
            .collect::<Vec<_>>(),
        [1, 5],
    );
    for projection in &repeated_panes {
        assert_eq!(projection.pane().pane_id(), &repeated_pane_id);
        assert_eq!(
            projection.link_identity().server_identity(),
            server.identity()
        );
        assert_eq!(
            projection.link_identity().session_id(),
            &repeated_session_id
        );
        assert_eq!(projection.link_identity().window_id(), &repeated_window_id);
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[cfg(feature = "test-support")]
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one isolated fixture proves grouped aggregate semantics end to end"
)]
async fn real_tmux_compat_aggregate_grouped_window_linked_tracks_release_semantics() {
    let (guard, executable, explicit) = projection_test_server().await;
    let server = guard.server();
    if let Some(expected) = explicit {
        assert!(
            executable == expected,
            "the selected executable matches LIBTMUX_TEST_TMUX",
        );
    }
    assert!(
        server.tmux_executable() == executable.as_os_str(),
        "the Server retains the selected tmux executable",
    );

    projection_run(
        server,
        Command::new("new-session")
            .arg("-d")
            .arg("-s")
            .arg("group-a")
            .arg("-n")
            .arg("shared")
            .arg("sleep 120"),
    )
    .await;
    projection_run(
        server,
        Command::new("new-session")
            .arg("-d")
            .arg("-s")
            .arg("group-b")
            .arg("-t")
            .arg("group-a"),
    )
    .await;

    let first_session_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("group-a")
            .arg("#{session_id}"),
    )
    .await
    .trim()
    .parse::<SessionId>()
    .expect("the first group Session ID is valid");
    let second_session_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("group-b")
            .arg("#{session_id}"),
    )
    .await
    .trim()
    .parse::<SessionId>()
    .expect("the second group Session ID is valid");
    let shared_window_id = projection_stdout(
        server,
        Command::new("display-message")
            .arg("-p")
            .arg("-t")
            .arg("group-a:0")
            .arg("#{window_id}"),
    )
    .await
    .trim()
    .parse::<WindowId>()
    .expect("the shared Window ID is valid");
    let version = server
        .capabilities()
        .await
        .expect("tmux capabilities are detected")
        .tmux_version();
    let expected_linked = projection_uses_post_3_6_semantics(version);
    let window_plan = window_projection_plan(version)
        .ok()
        .expect("the detected Window projection plan is valid");
    let result = server
        .cmd(
            Command::new("list-windows")
                .arg("-a")
                .arg("-F")
                .arg(window_plan.template()),
        )
        .await
        .expect("the grouped Window projection listing executes");
    assert!(result.success(), "the grouped Window listing succeeds");
    assert!(
        result.stderr().is_empty(),
        "the grouped Window stderr is empty"
    );
    let windows =
        hydrate_window_projections_from_stdout(server.identity(), &window_plan, result.stdout())
            .ok()
            .expect("the grouped Window stdout parses and hydrates");
    let grouped_rows: Vec<_> = windows
        .iter()
        .filter(|projection| {
            projection.window().window_id().as_ref() == shared_window_id.as_ref()
                && (projection.link().identity().session_id() == &first_session_id
                    || projection.link().identity().session_id() == &second_session_id)
        })
        .collect();

    assert_eq!(grouped_rows.len(), 2);
    let mut actual_session_ids = grouped_rows
        .iter()
        .map(|projection| projection.link().identity().session_id().clone())
        .collect::<Vec<_>>();
    actual_session_ids.sort();
    let mut expected_session_ids = vec![first_session_id, second_session_id];
    expected_session_ids.sort();
    assert_eq!(actual_session_ids, expected_session_ids);
    for projection in grouped_rows {
        assert_eq!(
            projection.link().identity().server_identity(),
            server.identity()
        );
        assert_eq!(projection.link().identity().window_id(), &shared_window_id);
        assert_eq!(projection.window().window_id(), &shared_window_id);
        assert_eq!(projection.link().is_linked(), expected_linked);
    }

    guard.shutdown().await.expect("tmux fixture shuts down");
}
