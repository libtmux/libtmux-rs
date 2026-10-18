//! Ordering and component contracts for numbered tmux releases.

use libtmux::{ReleaseSuffix, ReleaseVersion};

#[test]
fn releases_compare_numeric_components_then_suffixes() {
    let candidate = ReleaseSuffix::release_candidate(2).expect("nonzero candidate");
    let patch = ReleaseSuffix::patch('b').expect("lowercase patch");
    let ordered = [
        ReleaseVersion::new(3, 9, ReleaseSuffix::FINAL),
        ReleaseVersion::new(3, 10, ReleaseSuffix::RELEASE_CANDIDATE),
        ReleaseVersion::new(3, 10, candidate),
        ReleaseVersion::new(3, 10, ReleaseSuffix::FINAL),
        ReleaseVersion::new(3, 10, ReleaseSuffix::PATCH_A),
        ReleaseVersion::new(3, 10, patch),
        ReleaseVersion::new(4, 0, ReleaseSuffix::FINAL),
    ];
    assert!(ordered.windows(2).all(|pair| pair[0] < pair[1]));
    assert_eq!(ordered[1].to_string(), "3.10-rc");
    assert_eq!(ordered[2].to_string(), "3.10-rc2");
    assert_eq!(ordered[3].to_string(), "3.10");
    assert_eq!(ordered[4].to_string(), "3.10a");
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
