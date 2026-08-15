//! Integration tests for tmux version parsing and ordering.

use std::error::Error as StdError;

use libtmux::{Error, ReleaseSuffix, ReleaseVersion, TmuxVersion};
use static_assertions::assert_impl_all;

assert_impl_all!(Error: StdError, Send, Sync);
assert_impl_all!(ReleaseSuffix: Send, Sync);
assert_impl_all!(ReleaseVersion: Send, Sync);
assert_impl_all!(TmuxVersion: Send, Sync);

#[test]
fn parses_release_and_development_versions() {
    let patch_a = ReleaseSuffix::patch('a').expect("a is a lowercase patch suffix");
    let patch_b = ReleaseSuffix::patch('b').expect("b is a lowercase patch suffix");
    let patch_c = ReleaseSuffix::patch('c').expect("c is a lowercase patch suffix");
    let cases: &[(&[u8], &str, Option<ReleaseVersion>)] = &[
        (
            b"tmux 3.2\n",
            "3.2",
            Some(ReleaseVersion::new(3, 2, ReleaseSuffix::FINAL)),
        ),
        (
            b"renamed-tmux 3.2a\n",
            "3.2a",
            Some(ReleaseVersion::new(3, 2, patch_a)),
        ),
        (
            b"\xfftmux 3.2a\n",
            "3.2a",
            Some(ReleaseVersion::new(3, 2, patch_a)),
        ),
        (
            b"tmux 3.2a\n",
            "3.2a",
            Some(ReleaseVersion::new(3, 2, patch_a)),
        ),
        (
            b"tmux 3.1c\n",
            "3.1c",
            Some(ReleaseVersion::new(3, 1, patch_c)),
        ),
        (
            b"tmux 3.7b\n",
            "3.7b",
            Some(ReleaseVersion::new(3, 7, patch_b)),
        ),
        (b"tmux master\n", "master", None),
        (b"tmux next-3.8\n", "next-3.8", None),
    ];

    for (output, raw, release) in cases {
        let parsed = TmuxVersion::parse_output(output).expect("fixture is valid");
        assert_eq!(parsed.raw(), *raw);
        assert_eq!(parsed.release(), release.as_ref());
        assert_eq!(parsed.is_development(), release.is_none());
    }
}

#[test]
fn parses_release_suffixes_without_losing_order() {
    let rc = TmuxVersion::parse_output(b"tmux 3.7-rc\n").unwrap();
    let rc1 = TmuxVersion::parse_output(b"tmux 3.7-rc1\n").unwrap();
    let rc2 = TmuxVersion::parse_output(b"tmux 3.7-rc2\n").unwrap();
    let rc3 = TmuxVersion::parse_output(b"tmux 3.7-rc3\n").unwrap();
    let rc5 = TmuxVersion::parse_output(b"tmux 3.2-rc5\n").unwrap();
    let rc_max = TmuxVersion::parse_output(b"tmux 3.7-rc65535\n").unwrap();
    let release = TmuxVersion::parse_output(b"tmux 3.7\n").unwrap();
    let patch_a = TmuxVersion::parse_output(b"tmux 3.7a\n").unwrap();
    let patch_b = TmuxVersion::parse_output(b"tmux 3.7b\n").unwrap();

    assert_eq!(rc, rc1);
    assert!(rc1 < rc2);
    assert!(rc2 < rc3);
    assert!(rc3 < release);
    assert!(release < patch_a);
    assert!(patch_a < patch_b);
    assert_eq!(
        rc5.release()
            .expect("fixture is a release")
            .suffix()
            .release_candidate_number(),
        Some(5),
    );
    assert_eq!(
        rc_max
            .release()
            .expect("fixture is a release")
            .suffix()
            .release_candidate_number(),
        Some(u16::MAX),
    );
}

#[test]
fn compares_numeric_release_components() {
    let one_nine = TmuxVersion::parse_output(b"tmux 1.9\n").unwrap();
    let one_ten = TmuxVersion::parse_output(b"tmux 1.10\n").unwrap();

    assert!(one_nine < one_ten);
}

#[test]
fn release_display_uses_canonical_suffixes() {
    let cases: &[(&[u8], &str)] = &[
        (b"tmux 3.7-rc\n", "3.7-rc"),
        (b"tmux 3.7-rc1\n", "3.7-rc"),
        (b"tmux 3.7-rc2\n", "3.7-rc2"),
        (b"tmux 3.7\n", "3.7"),
        (b"tmux 3.7a\n", "3.7a"),
        (b"tmux 1.10\n", "1.10"),
    ];

    for (output, expected) in cases {
        let version = TmuxVersion::parse_output(output).expect("fixture is valid");
        let release = version.release().expect("fixture is a numbered release");
        assert_eq!(release.to_string(), *expected);
    }
}

#[test]
fn development_versions_do_not_invent_release_ordering() {
    let master = TmuxVersion::parse_output(b"tmux master\n").unwrap();
    let release = TmuxVersion::parse_output(b"tmux 3.7\n").unwrap();

    assert_eq!(master.partial_cmp(&release), None);
    assert_eq!(release.partial_cmp(&master), None);
}

#[test]
fn rejects_malformed_output_and_reports_only_its_length() {
    let cases: &[&[u8]] = &[
        b"3.7\n",
        b"tmux 3.7\nextra\n",
        b"tmux \xff\n",
        b"tmux 65536.1\n",
        b"tmux 3.65536\n",
        b"tmux 03.2\n",
        b"tmux 3.02\n",
        b"tmux 3.7A\n",
        b"tmux 3.7a trailing\n",
        b"tmux 3.7aa\n",
        b"tmux 3.7-rc0\n",
        b"tmux 3.7-rc01\n",
        b"tmux 3.7-rc65536\n",
        b"tmux 3.7\r\n",
        b"tmux 3.7",
        b"tmux \n",
        b" 3.7\n",
        b"tmux  3.7\n",
        b"tmux\t3.7\n",
        b"tmux 3.7 extra\n",
        b"tmux 3.7 \n",
        b"tm\x01ux 3.7\n",
        b"tmux \x013.7\n",
        b"tmux unknown\n",
        b"tmux next-\n",
        b"tmux next-3\n",
        b"tmux next-3.8a\n",
        b"tmux next-3..8\n",
        b"tmux next--3.8\n",
        b"tmux next-3-8\n",
        b"tmux next-65536.8\n",
        b"tmux next-3.65536\n",
        b"tmux next-03.3\n",
        b"tmux next-3.03\n",
        b"tmux next-3.\xc3\xa9\n",
    ];

    for output in cases {
        let error = TmuxVersion::parse_output(output).expect_err("fixture is malformed");
        assert_eq!(error.invalid_version_output_len(), Some(output.len()));
    }
}

#[test]
fn invalid_output_is_redacted_from_diagnostics() {
    let output = b"tmux secret-SENTINEL\n";
    let error = TmuxVersion::parse_output(output).expect_err("fixture is malformed");

    assert_eq!(error.invalid_version_output_len(), Some(output.len()));

    let mut cause: Option<&(dyn StdError + 'static)> = Some(&error);
    while let Some(current) = cause {
        assert!(!current.to_string().contains("SENTINEL"));
        assert!(!format!("{current:?}").contains("SENTINEL"));
        cause = current.source();
    }
}

#[test]
fn enforces_the_minimum_without_promoting_development_versions() {
    let old = TmuxVersion::parse_output(b"tmux 3.2\n").unwrap();
    let minimum = TmuxVersion::parse_output(b"tmux 3.2a\n").unwrap();
    let master = TmuxVersion::parse_output(b"tmux master\n").unwrap();
    let next_3_2 = TmuxVersion::parse_output(b"tmux next-3.2\n").unwrap();
    let next_3_3 = TmuxVersion::parse_output(b"tmux next-3.3\n").unwrap();
    let next_3_8 = TmuxVersion::parse_output(b"tmux next-3.8\n").unwrap();
    let newer_requirement = ReleaseVersion::new(
        3,
        7,
        ReleaseSuffix::patch('b').expect("b is a lowercase patch suffix"),
    );

    assert!(!old.meets(&TmuxVersion::MIN_SUPPORTED));
    assert!(minimum.meets(&TmuxVersion::MIN_SUPPORTED));
    assert!(master.meets(&TmuxVersion::MIN_SUPPORTED));
    assert!(!master.meets(&newer_requirement));
    assert!(!next_3_2.meets(&TmuxVersion::MIN_SUPPORTED));
    assert!(next_3_3.meets(&TmuxVersion::MIN_SUPPORTED));
    assert!(!next_3_3.meets(&newer_requirement));
    assert!(next_3_8.meets(&TmuxVersion::MIN_SUPPORTED));
    assert!(!next_3_8.meets(&newer_requirement));

    let error = old.ensure_supported().expect_err("3.2 is below 3.2a");
    match &error {
        Error::UnsupportedTmuxVersion { found, minimum } => {
            assert_eq!(found, &old);
            assert_eq!(minimum, &TmuxVersion::MIN_SUPPORTED);
        }
        _ => panic!("expected an unsupported tmux version error"),
    }
    assert_eq!(error.found_version(), Some(&old));
    assert_eq!(error.minimum_version(), Some(&TmuxVersion::MIN_SUPPORTED));
    assert!(minimum.ensure_supported().is_ok());
}

#[test]
fn release_value_getters_preserve_components() {
    let suffix = ReleaseSuffix::patch('c').expect("c is a lowercase patch suffix");
    let release = ReleaseVersion::new(3, 1, suffix);

    assert_eq!(release.major(), 3);
    assert_eq!(release.minor(), 1);
    assert_eq!(release.suffix(), suffix);
    assert_eq!(suffix.patch_letter(), Some('c'));
    assert!(!suffix.is_final());
    assert!(!suffix.is_release_candidate());
    assert!(ReleaseSuffix::FINAL.is_final());
    assert!(ReleaseSuffix::RELEASE_CANDIDATE.is_release_candidate());
    assert_eq!(
        ReleaseSuffix::release_candidate(2)
            .expect("two is a valid release candidate")
            .release_candidate_number(),
        Some(2),
    );
    assert_eq!(
        ReleaseSuffix::RELEASE_CANDIDATE.release_candidate_number(),
        Some(1),
    );
    assert_eq!(ReleaseSuffix::release_candidate(0), None);
    assert_eq!(ReleaseSuffix::patch('A'), None);
    assert_eq!(ReleaseSuffix::patch('\u{00e9}'), None);
}

#[test]
fn error_is_a_thread_safe_static_standard_error() {
    fn assert_error<T: StdError + Send + Sync + 'static>() {}

    assert_error::<Error>();
}
