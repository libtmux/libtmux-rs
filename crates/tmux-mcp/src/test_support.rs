//! Native eligibility for tests that acquire a control observer.

use libtmux::TmuxVersion;
use libtmux::test::TestServer;

pub(crate) async fn control_fixture(guard: TestServer) -> Option<TestServer> {
    let report = guard
        .server()
        .format(None, "tmux #{version}")
        .await
        .expect("daemon release");
    let mut report = report.as_bytes().to_vec();
    report.push(b'\n');
    let version = TmuxVersion::parse_output(&report).expect("daemon release parses");
    if version.has_behavior(&libtmux::since::CONTROL_ACQUISITION) {
        Some(guard)
    } else {
        guard
            .shutdown()
            .await
            .expect("ineligible control fixture closes");
        None
    }
}
