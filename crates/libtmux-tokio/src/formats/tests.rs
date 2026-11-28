#![allow(clippy::err_expect, clippy::expect_used, clippy::ok_expect)]

#[cfg(feature = "test-support")]
use super::{DecoderKind, FormatDescriptor, FormatPlan, TransportDialect, decode_text};
#[cfg(feature = "test-support")]
use crate::{Command, ReleaseSuffix, ReleaseVersion, test::TestServer};

#[cfg(feature = "test-support")]
static RAW_FORMAT_BYTES: FormatDescriptor =
    FormatDescriptor::for_codec_test("@libtmux_format_bytes", DecoderKind::Text);

#[cfg(feature = "test-support")]
async fn format_compat_test_server() -> (TestServer, std::ffi::OsString) {
    let executable =
        std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| std::ffi::OsString::from("tmux"));
    let guard = TestServer::builder()
        .tmux_executable(executable.clone())
        .start()
        .await
        .expect("the explicitly selected tmux starts");
    assert!(
        guard.server().tmux_executable() == executable.as_os_str(),
        "TestServer retains the selected tmux executable",
    );
    (guard, executable)
}

#[cfg(feature = "test-support")]
async fn format_compat_run(server: &crate::Server, command: Command) {
    let result = server
        .cmd(command)
        .await
        .expect("tmux fixture command executes");
    assert!(result.success(), "tmux fixture command succeeds");
    assert!(result.stderr().is_empty(), "tmux fixture stderr is empty");
}

#[cfg(feature = "test-support")]
#[tokio::test]
async fn real_tmux_compat_format_q_matches_versioned_adversarial_option_transport() {
    use std::os::unix::ffi::OsStringExt as _;

    const OPTION_BYTES: [u8; 9] = [0x3a, 0x0a, 0x3d, 0x5c, 0xe2, 0x90, 0x9e, 0x80, 0xff];
    const EXPECTED_RAW_STDOUT: [u8; 13] = [
        0x3a, 0x0a, 0x5c, 0x3d, 0x5c, 0x5c, 0xe2, 0x90, 0x9e, 0x80, 0xff, 0x3d, 0x0a,
    ];
    // 3.8 added the newline to the quoted set, so it arrives escaped.
    const EXPECTED_ESCAPED_NEWLINE_STDOUT: [u8; 14] = [
        0x3a, 0x5c, 0x0a, 0x5c, 0x3d, 0x5c, 0x5c, 0xe2, 0x90, 0x9e, 0x80, 0xff, 0x3d, 0x0a,
    ];
    const ESCAPED_NEWLINE: ReleaseVersion = ReleaseVersion::new(3, 8, ReleaseSuffix::FINAL);
    const EXPECTED_VIS_STDOUT: [u8; 19] = [
        0x3a, 0x0a, 0x5c, 0x3d, 0x5c, 0x5c, 0xe2, 0x90, 0x9e, 0x5c, 0x32, 0x30, 0x30, 0x5c, 0x33,
        0x37, 0x37, 0x3d, 0x0a,
    ];

    let (guard, executable) = format_compat_test_server().await;
    let server = guard.server();
    if let Some(expected) = std::env::var_os("LIBTMUX_TEST_TMUX") {
        assert!(
            executable == expected,
            "the selected executable matches LIBTMUX_TEST_TMUX",
        );
    }
    assert!(
        server.tmux_executable() == executable.as_os_str(),
        "the Server retains the selected tmux executable",
    );

    format_compat_run(
        server,
        Command::new("new-session")
            .arg("-d")
            .arg("-s")
            .arg("format-bytes")
            .arg("sleep 120"),
    )
    .await;
    format_compat_run(
        server,
        Command::new("set-option")
            .arg("-g")
            .arg(RAW_FORMAT_BYTES.name())
            .sensitive_arg(std::ffi::OsString::from_vec(OPTION_BYTES.to_vec())),
    )
    .await;

    let plan = FormatPlan::for_codec_test(vec![&RAW_FORMAT_BYTES]).expect("fixture plan");
    assert_eq!(plan.template(), "#{q:@libtmux_format_bytes}=");
    assert_eq!(
        plan.template()
            .matches("#{q:@libtmux_format_bytes}")
            .count(),
        1,
    );
    let result = server
        .cmd(Command::new("list-sessions").arg("-F").arg(plan.template()))
        .await
        .expect("the raw format listing executes");
    assert!(result.success(), "the raw format listing succeeds");
    assert!(result.stderr().is_empty(), "the raw format stderr is empty");
    let version = server
        .capabilities()
        .await
        .expect("tmux capabilities are detected")
        .tmux_version();
    // A numbered release's wire is frozen forever, so its exact bytes are
    // worth pinning; a development identifier's is not -- its escape set
    // can still widen, so only the decode claim below is asserted for it.
    let frozen = version.release().is_some();
    match TransportDialect::for_version(version) {
        TransportDialect::Vis => {
            if frozen {
                assert_eq!(result.stdout(), EXPECTED_VIS_STDOUT);
            }
            // The visual encoding makes the transport valid UTF-8 even
            // though the underlying value is not.
            assert!(result.stdout_utf8().is_ok());
        }
        TransportDialect::RawQ => {
            if frozen && version.has_behavior(&ESCAPED_NEWLINE) {
                assert_eq!(result.stdout(), EXPECTED_ESCAPED_NEWLINE_STDOUT);
            } else if frozen {
                assert_eq!(result.stdout(), EXPECTED_RAW_STDOUT);
            }
            assert!(result.stdout_utf8().is_err());
        }
    }

    // The decoded value is the same on every dialect and every release,
    // frozen or not. This is the claim the crate makes to callers, so it is
    // the one assertion this test never conditions away.
    let versioned = FormatPlan::for_codec_test_at(vec![&RAW_FORMAT_BYTES], version)
        .ok()
        .expect("a plan exists for the detected version");
    let parsed = versioned
        .parse_rows(result.stdout())
        .ok()
        .expect("the owning plan parses borrowed live stdout");
    assert_eq!(parsed.len(), 1);
    let mut slots = parsed[0].slots();
    assert_eq!(slots.len(), 1);
    let slot = slots.next().expect("the one format slot exists");
    assert!(std::ptr::eq(
        std::ptr::from_ref(slot.descriptor()),
        std::ptr::addr_of!(RAW_FORMAT_BYTES),
    ));
    assert_eq!(decode_text(slot).as_bytes(), OPTION_BYTES);
    assert!(slots.next().is_none());

    guard.shutdown().await.expect("tmux fixture shuts down");
}

/// Record which format names each listing asks tmux for.
///
/// The supplement arrays are macro-generated, so the answer exists in the
/// source and cannot be read out of it: the only way to learn what a
/// listing requests has been to intercept the `-F` string of a running
/// command. This writes the answer down and fails when it drifts.
///
/// Regenerate with `just list-profiles`.
#[test]
fn each_listing_records_the_fields_it_requests() {
    use super::{FormatPlan, ListProfile};
    use crate::snapshot::{pane_projection_plan, window_projection_plan};
    use crate::version::TmuxVersion;
    use std::fmt::Write as _;

    let version = TmuxVersion::parse_output(b"tmux 3.7\n").expect("a supported release");
    let mut recorded = String::from(
        "# Which format names each listing asks tmux for, at the newest\n\
         # release the lanes build. The supplement arrays this comes from\n\
         # are macro-generated; without this file the only way to learn a\n\
         # listing's fields is to intercept its `-F` string.\n\
         #\n\
         # Recorded from the plan each listing builds, which is not the\n\
         # same for all four: sessions and clients plan from the profile,\n\
         # windows and panes from a requested set that chains a projection\n\
         # suffix onto it. What this cannot see is a listing that changes\n\
         # which plan it builds -- the check would then hold this file\n\
         # consistent with itself rather than with the `-F` string. The\n\
         # cross-check is to intercept the argv and count.\n\
         #\n\
         # Regenerate with `just list-profiles`.\n\n",
    );
    // Built the way each listing builds it, not the way they look alike.
    // `sessions` and `clients` plan from the profile; `windows` and
    // `panes` plan from a requested set that chains a projection suffix
    // onto the supplements, and recording the profile plan for those two
    // documented a template nothing sends.
    let plans = [
        (
            "sessions",
            FormatPlan::for_profile(ListProfile::Sessions, &version),
        ),
        (
            "windows",
            window_projection_plan(&version).expect("a supported release plans windows"),
        ),
        (
            "panes",
            pane_projection_plan(&version).expect("a supported release plans panes"),
        ),
        (
            "clients",
            FormatPlan::for_profile(ListProfile::Clients, &version),
        ),
    ];
    for (index, (label, plan)) in plans.into_iter().enumerate() {
        if index > 0 {
            recorded.push('\n');
        }
        let mut names: Vec<&str> = plan
            .descriptors_for_test()
            .iter()
            .map(|descriptor| descriptor.name())
            .collect();
        names.sort_unstable();
        let _ = writeln!(recorded, "{label} ({} fields)", names.len());
        for name in names {
            let _ = writeln!(recorded, "  {name}");
        }
    }

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/docs/list-profiles.txt");
    if std::env::var_os("LIBTMUX_RERECORD").is_some() {
        std::fs::write(path, &recorded).expect("the ledger is writable");
        return;
    }
    let stored = std::fs::read_to_string(path).unwrap_or_default();
    assert_eq!(
        stored, recorded,
        "the fields a listing requests changed; run `just list-profiles`",
    );
}
