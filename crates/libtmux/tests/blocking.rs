//! The blocking runtime, driving the async API from ordinary code.

#![cfg(all(feature = "blocking", feature = "test-support"))]

use libtmux::blocking::Runtime;
use libtmux::test::TestServer;

#[test]
fn a_script_can_drive_the_whole_api_without_being_async() {
    let runtime = Runtime::new().expect("a runtime is built");

    // Everything goes through one runtime, including setup and teardown, so
    // there is no async main anywhere in this test.
    let guard = runtime
        .run(TestServer::builder().start())
        .expect("tmux starts");
    let server = guard.server();

    assert!(runtime.run(server.sessions()).expect("list").is_empty());

    let session = runtime
        .run(server.new_session("blocking"))
        .expect("session is created");
    assert_eq!(session.name().as_bytes(), b"blocking",);

    // A method added later needs no new wrapper: the runtime runs any future.
    let window = runtime
        .run(session.new_window("work"))
        .expect("window is created");
    assert_eq!(runtime.run(window.panes()).expect("panes").len(), 1);

    let found = runtime
        .run(server.session("blocking"))
        .expect("lookup")
        .expect("the session exists");
    assert_eq!(found, session);

    runtime
        .run(guard.shutdown())
        .expect("tmux fixture shuts down");
}

#[test]
fn one_runtime_serves_several_servers() {
    let runtime = Runtime::new().expect("a runtime is built");

    let first = runtime
        .run(TestServer::builder().start())
        .expect("tmux starts");
    let second = runtime
        .run(TestServer::builder().start())
        .expect("tmux starts");

    runtime
        .run(first.server().new_session("one"))
        .expect("session is created");

    // Separate servers stay separate; the shared runtime does not merge them.
    assert_eq!(
        runtime.run(first.server().sessions()).expect("list").len(),
        1
    );
    assert!(
        runtime
            .run(second.server().sessions())
            .expect("list")
            .is_empty()
    );

    runtime.run(first.shutdown()).expect("first shuts down");
    runtime.run(second.shutdown()).expect("second shuts down");
}

/// The async-io driver has no Tokio runtime to tear down.
#[test]
fn the_async_io_runtime_can_run_inside_an_unrelated_async_context() {
    let runtime = Runtime::new().expect("a runtime is built outside async");

    let driver = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a driver runtime");

    let reported = driver.block_on(async move { runtime.try_run(async { 2 + 2 }) });

    assert_eq!(
        reported.expect("the async-io executor has no Tokio nesting restriction"),
        4
    );
}
