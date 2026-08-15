//! Public contract tests for byte-preserving tmux text.

use std::borrow::Cow;
use std::fmt::{Debug, Display};
use std::hash::{Hash, Hasher};
use std::ops::Deref;

use libtmux::TmuxText;
use static_assertions::{assert_impl_all, assert_not_impl_any};

assert_impl_all!(TmuxText: Clone, Debug, Eq, Hash, Ord, Send, Sync);
assert_not_impl_any!(TmuxText: Display, Deref<Target = str>, AsRef<str>, Into<String>);

struct IntoBytes([u8; 2]);

impl From<IntoBytes> for Vec<u8> {
    fn from(value: IntoBytes) -> Self {
        value.0.into()
    }
}

#[derive(Default)]
struct RecordingHasher {
    writes: Vec<Vec<u8>>,
}

impl Hasher for RecordingHasher {
    fn finish(&self) -> u64 {
        0
    }

    fn write(&mut self, bytes: &[u8]) {
        self.writes.push(bytes.to_vec());
    }
}

fn hash_writes(value: &TmuxText) -> Vec<Vec<u8>> {
    let mut hasher = RecordingHasher::default();
    value.hash(&mut hasher);
    hasher.writes
}

#[test]
fn constructors_preserve_exact_utf8_and_arbitrary_bytes() {
    let borrowed_source = String::from("\u{e9}");
    let borrowed: TmuxText = borrowed_source.as_str().into();
    let owned: TmuxText = String::from("e\u{301}").into();
    let vector: TmuxText = Vec::from([b'c', 0, 0xff]).into();
    let raw = TmuxText::from_bytes(IntoBytes([b'd', 0xfe]));
    let empty = TmuxText::from_bytes(Vec::new());
    let as_bytes: for<'a> fn(&'a TmuxText) -> &'a [u8] = TmuxText::as_bytes;
    let raw_bytes = as_bytes(&raw);

    assert_eq!(borrowed.as_bytes(), [0xc3, 0xa9]);
    assert_eq!(owned.as_bytes(), [b'e', 0xcc, 0x81]);
    assert_ne!(borrowed, owned);
    assert_eq!(vector.as_bytes(), [b'c', 0, 0xff]);
    assert_eq!(raw_bytes, [b'd', 0xfe]);
    assert!(empty.as_bytes().is_empty());
}

#[test]
fn explicit_views_prevent_implicit_lossy_conversion() {
    let valid = TmuxText::from("exact text");
    let strict_result: Result<&str, std::str::Utf8Error> = valid.as_str();
    let strict = strict_result.expect("UTF-8 fixture is valid");
    assert_eq!(strict, "exact text");
    assert!(std::ptr::eq(strict.as_ptr(), valid.as_bytes().as_ptr()));

    let invalid = TmuxText::from_bytes(Vec::from([b'a', 0xff, b'b']));
    let error = invalid
        .as_str()
        .expect_err("arbitrary tmux bytes need not be UTF-8");
    assert_eq!(error.valid_up_to(), 1);
    assert_eq!(error.error_len(), Some(1));
    let lossy: Cow<'_, str> = invalid.to_string_lossy();
    assert_eq!(lossy, "a\u{fffd}b");
    assert_eq!(invalid.as_bytes(), [b'a', 0xff, b'b']);
}

#[test]
fn bytewise_traits_reject_unicode_based_equality_and_ordering() {
    let lower = TmuxText::from_bytes(Vec::from([0x80]));
    let higher = TmuxText::from_bytes(Vec::from([0x81]));

    assert_eq!(lower.to_string_lossy(), higher.to_string_lossy());
    assert_ne!(lower, higher);
    assert!(lower < higher);
    assert_ne!(hash_writes(&lower), hash_writes(&higher));

    let cloned = lower.clone();
    assert_eq!(lower, cloned);
    assert_eq!(hash_writes(&lower), hash_writes(&cloned));
}

#[test]
fn debug_omits_payload_bytes_and_lengths() {
    let short_secret = TmuxText::from("alpha-secret");
    let long_secret = TmuxText::from("bravo-value-with-a-distinct-length");
    let invalid_secret = TmuxText::from_bytes([b'c', b'h', b'a', b'r', 0xff]);
    let empty = TmuxText::from_bytes(Vec::new());
    let short_debug = format!("{short_secret:?}");
    let long_debug = format!("{long_secret:?}");
    let invalid_debug = format!("{invalid_secret:?}");
    let empty_debug = format!("{empty:?}");

    assert_eq!(short_debug, long_debug);
    assert_eq!(short_debug, invalid_debug);
    assert_eq!(short_debug, empty_debug);
    assert!(!short_debug.is_empty());
    assert!(!short_debug.contains("alpha"));
    assert!(!long_debug.contains("bravo"));
    assert!(!short_debug.contains(&short_secret.as_bytes().len().to_string()));
    assert!(!long_debug.contains(&long_secret.as_bytes().len().to_string()));
    assert!(!invalid_debug.contains(&invalid_secret.as_bytes().len().to_string()));
    assert!(!empty_debug.contains(&empty.as_bytes().len().to_string()));
}
