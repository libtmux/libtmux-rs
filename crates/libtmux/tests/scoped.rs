//! Scoped operations retain the caller's error when cleanup also fails.

#![cfg(feature = "test-support")]

use std::rc::Rc;

use libtmux::test::TestServer;
use libtmux::{Error, ErrorKind, NewWindowOptions, ScopeError, SplitDirection, SplitOptions};

struct OperationFailure(Rc<()>);

#[tokio::test]
async fn cleanup_failure_retains_the_owned_operation_error() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let witness = Rc::new(());
    let result = guard
        .server()
        .with_session("scope-errors", async |session| {
            session.clone().kill().await.expect("session is killed");
            Err::<(), _>(OperationFailure(Rc::clone(&witness)))
        })
        .await;
    guard.shutdown().await.expect("tmux fixture shuts down");
    assert_eq!(
        Rc::strong_count(&witness),
        2,
        "the result must retain the original operation error after cleanup fails"
    );
    assert!(format!("{result:?}").contains("<redacted>"));
    assert_combined(result.expect_err("operation and cleanup fail"), &witness);
    assert_eq!(Rc::strong_count(&witness), 1);
}

#[allow(clippy::panic, reason = "test assertion helper")]
fn assert_combined(error: ScopeError<OperationFailure>, witness: &Rc<()>) {
    let ScopeError::OperationAndCleanup { operation, cleanup } = error else {
        panic!("both errors must be preserved");
    };
    assert!(Rc::ptr_eq(&operation.0, witness));
    assert_eq!(cleanup.kind(), ErrorKind::PartialEffect);
    assert!(matches!(cleanup, Error::AfterEffect { source, .. } if source.is_object_gone()));
}

#[tokio::test]
async fn window_and_pane_scopes_preserve_both_errors() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let session = guard.session("scopes").await.expect("session");
    let witness = Rc::new(());
    let error = session
        .with_window(
            NewWindowOptions::new("temporary").command("sleep 300"),
            async |window| {
                window.clone().kill().await.expect("window is killed");
                Err::<(), _>(OperationFailure(Rc::clone(&witness)))
            },
        )
        .await
        .expect_err("operation and cleanup fail");
    assert_combined(error, &witness);

    let window = session
        .active_window()
        .await
        .expect("window lookup")
        .expect("window");
    let error = window
        .with_pane(
            SplitOptions::new(SplitDirection::Below).command("sleep 300"),
            async |pane| {
                pane.clone().kill().await.expect("pane is killed");
                Err::<(), _>(OperationFailure(Rc::clone(&witness)))
            },
        )
        .await
        .expect_err("operation and cleanup fail");
    assert_combined(error, &witness);
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn creation_failure_does_not_run_the_operation_or_adopt_an_existing_session() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let original = guard.session("existing").await.expect("session");
    let called = std::cell::Cell::new(false);
    let error = guard
        .server()
        .with_session("existing", async |_session| {
            called.set(true);
            Ok::<(), OperationFailure>(())
        })
        .await
        .expect_err("the existing session cannot be created twice");
    assert!(matches!(error, ScopeError::Creation(_)));
    assert!(!called.get());
    assert_eq!(
        guard.server().sessions().await.expect("sessions"),
        [original]
    );
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn cleanup_failure_after_success_is_a_partial_effect() {
    let guard = TestServer::builder().start().await.expect("tmux starts");
    let error = guard
        .server()
        .with_session("cleanup", async |session| {
            session.clone().kill().await.expect("session is killed");
            Ok::<(), OperationFailure>(())
        })
        .await
        .expect_err("cleanup fails");
    assert!(matches!(
        error,
        ScopeError::Cleanup(Error::AfterEffect { .. })
    ));
    guard.shutdown().await.expect("tmux fixture shuts down");
}

#[tokio::test]
async fn combined_errors_keep_typed_sources_and_redact_operation_details() {
    use std::error::Error as _;

    let guard = TestServer::builder().start().await.expect("tmux starts");
    let error = guard
        .server()
        .with_session("redaction", async |session| {
            session.clone().kill().await.expect("session is killed");
            Err::<(), _>(std::io::Error::other("operation-secret"))
        })
        .await
        .expect_err("operation and cleanup fail");
    guard.shutdown().await.expect("tmux fixture shuts down");
    assert!(!format!("{error:?} {error}").contains("operation-secret"));
    let cleanup_source = error.source().expect("cleanup source");
    assert!(cleanup_source.is::<Error>());
    let ScopeError::OperationAndCleanup { operation, cleanup } = &error else {
        panic!("combined error");
    };
    assert_eq!(operation.to_string(), "operation-secret");
    assert!(std::ptr::eq(
        cleanup_source
            .downcast_ref::<Error>()
            .expect("typed cleanup"),
        cleanup
    ));

    let error = ScopeError::Operation(std::io::Error::other("operation-secret"));
    assert!(!format!("{error:?} {error}").contains("operation-secret"));
    assert!(error.source().is_none());
    assert!(
        matches!(error, ScopeError::Operation(operation) if operation.to_string() == "operation-secret")
    );
}
