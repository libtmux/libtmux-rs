//! The complete client works from an ordinary synchronous function.

#![cfg(feature = "test-support")]

use libtmux::test::TestServer;

#[test]
fn a_script_can_drive_the_whole_api_without_being_async() {
    let guard = TestServer::builder().start().expect("tmux starts");
    let server = guard.server();

    assert!(server.sessions().expect("list").is_empty());
    let session = server.new_session("blocking").expect("session is created");
    assert_eq!(session.name().as_bytes(), b"blocking");

    let window = session.new_window("work").expect("window is created");
    assert_eq!(window.panes().expect("panes").len(), 1);
    let found = server
        .session("blocking")
        .expect("lookup")
        .expect("session exists");
    assert_eq!(found, session);

    guard.shutdown().expect("tmux fixture shuts down");
}

#[test]
fn one_thread_serves_several_servers() {
    let first = TestServer::builder().start().expect("tmux starts");
    let second = TestServer::builder().start().expect("tmux starts");

    first
        .server()
        .new_session("one")
        .expect("session is created");
    assert_eq!(first.server().sessions().expect("list").len(), 1);
    assert!(second.server().sessions().expect("list").is_empty());

    first.shutdown().expect("first shuts down");
    second.shutdown().expect("second shuts down");
}
