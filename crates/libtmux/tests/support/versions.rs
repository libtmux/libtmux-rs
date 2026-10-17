//! Native daemon eligibility for positive feature fixtures.

#![allow(dead_code, clippy::expect_used)]

use libtmux::test::TestServer;
use libtmux::{ReleaseVersion, Server, TmuxVersion};

async fn daemon_version(server: &Server) -> TmuxVersion {
    let report = server
        .format(None, "tmux #{version}")
        .await
        .expect("daemon release");
    let mut report = report.as_bytes().to_vec();
    report.push(b'\n');
    TmuxVersion::parse_output(&report).expect("numbered daemon release")
}

pub(crate) async fn supports(server: &Server, needs: &ReleaseVersion) -> bool {
    daemon_version(server).await.has_behavior(needs)
}

pub(crate) async fn supports_control(server: &Server) -> bool {
    daemon_version(server)
        .await
        .release()
        .is_some_and(|release| {
            *release >= libtmux::since::CONTROL_ACQUISITION
                && !release.suffix().is_release_candidate()
        })
}

pub(crate) async fn feature_fixture(
    guard: TestServer,
    needs: &ReleaseVersion,
) -> Option<TestServer> {
    if supports(guard.server(), needs).await {
        Some(guard)
    } else {
        eprintln!("positive feature fixture needs tmux {needs}");
        guard.shutdown().await.expect("ineligible fixture closes");
        None
    }
}

pub(crate) async fn control_fixture(guard: TestServer) -> Option<TestServer> {
    if supports_control(guard.server()).await {
        Some(guard)
    } else {
        eprintln!("positive control fixture needs stable tmux 3.7 or newer");
        guard.shutdown().await.expect("ineligible fixture closes");
        None
    }
}
