use std::collections::hash_map::DefaultHasher;
use std::ffi::{OsStr, OsString};
use std::hash::{Hash, Hasher};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use super::endpoint_resolution::{EndpointInputs, resolve_server_identity};
use super::{ServerIdentity, WindowLinkIdentity};
use crate::{SessionId, WindowId};
use static_assertions::{assert_impl_all, assert_not_impl_any};

fn hash(value: &ServerIdentity) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn inputs<'a>(
    cwd: &'a Path,
    socket_root: Option<&'a OsStr>,
    inherited_tmux: Option<&'a OsStr>,
) -> EndpointInputs<'a> {
    EndpointInputs::new(cwd, socket_root, 1000, inherited_tmux)
}

#[test]
fn captured_absolute_socket_paths_define_server_identity() {
    let cwd = Path::new("/tmp/work");
    let left = resolve_server_identity(
        Some(OsStr::new("relative/socket")),
        None,
        inputs(cwd, None, None),
    )
    .unwrap();
    let right = resolve_server_identity(
        Some(OsStr::new("/tmp/work/relative/socket")),
        None,
        inputs(Path::new("/unused"), None, None),
    )
    .unwrap();

    assert_eq!(left, right);
    assert_eq!(hash(&left), hash(&right));
    assert_eq!(left.socket_path(), Path::new("/tmp/work/relative/socket"));
}

#[test]
fn explicit_socket_paths_preserve_parent_components() {
    let cwd = Path::new("/tmp/work");
    let preserved = resolve_server_identity(
        Some(OsStr::new("relative/../socket")),
        None,
        inputs(cwd, None, None),
    )
    .unwrap();
    let collapsed =
        resolve_server_identity(Some(OsStr::new("socket")), None, inputs(cwd, None, None)).unwrap();

    assert_eq!(
        preserved.socket_path(),
        Path::new("/tmp/work/relative/../socket"),
    );
    assert_ne!(preserved, collapsed);
}

#[test]
fn explicit_socket_identity_preserves_raw_separator_spelling() {
    let cwd = Path::new("/work");
    let plain = resolve_server_identity(
        Some(OsStr::new("/tmp/socket")),
        None,
        inputs(cwd, None, None),
    )
    .unwrap();
    let trailing = resolve_server_identity(
        Some(OsStr::new("/tmp/socket/")),
        None,
        inputs(cwd, None, None),
    )
    .unwrap();
    let double_leading = resolve_server_identity(
        Some(OsStr::new("//tmp/socket")),
        None,
        inputs(cwd, None, None),
    )
    .unwrap();

    assert_ne!(plain, trailing);
    assert_ne!(plain, double_leading);
}

#[test]
fn named_and_default_sockets_include_the_resolved_root_and_real_uid() {
    let root = tempfile::tempdir().unwrap();
    // Resolution canonicalizes, which is what makes two selectors for one
    // endpoint compare equal. On macOS the temporary root reaches here as
    // `/var/...` and resolves to `/private/var/...`, so the expectation
    // has to be canonical too or it is only testing Linux.
    let canonical = root.path().canonicalize().unwrap();
    let context = inputs(Path::new("/work"), Some(root.path().as_os_str()), None);
    let named = resolve_server_identity(None, Some(OsStr::new("testing")), context).unwrap();
    let default = resolve_server_identity(None, None, context).unwrap();

    assert_eq!(named.socket_path(), canonical.join("tmux-1000/testing"),);
    assert_eq!(default.socket_path(), canonical.join("tmux-1000/default"),);
    assert_ne!(named, default);
}

#[test]
fn existing_relative_and_symlinked_socket_root_is_canonicalized() {
    let workspace = tempfile::tempdir().unwrap();
    let actual = workspace.path().join("actual");
    std::fs::create_dir(&actual).unwrap();
    symlink(&actual, workspace.path().join("link")).unwrap();
    let identity = resolve_server_identity(
        None,
        None,
        inputs(workspace.path(), Some(OsStr::new("link")), None),
    )
    .unwrap();

    assert_eq!(
        identity.socket_path(),
        actual.canonicalize().unwrap().join("tmux-1000/default"),
    );
}

#[test]
fn missing_empty_and_unset_socket_roots_fall_back_to_tmp() {
    let workspace = tempfile::tempdir().unwrap();
    let missing = workspace.path().join("missing");
    let candidates = [None, Some(OsStr::new("")), Some(missing.as_os_str())];
    let fallback = Path::new("/tmp").canonicalize().unwrap();

    for candidate in candidates {
        let identity =
            resolve_server_identity(None, None, inputs(workspace.path(), candidate, None)).unwrap();
        assert_eq!(identity.socket_path(), fallback.join("tmux-1000/default"),);
    }
}

#[test]
fn inherited_tmux_is_split_from_the_right() {
    let identity = resolve_server_identity(
        None,
        None,
        inputs(
            Path::new("/work"),
            None,
            Some(OsStr::new("/tmp/od,d/socket,84215,3")),
        ),
    )
    .unwrap();

    assert_eq!(identity.socket_path(), Path::new("/tmp/od,d/socket"));
}

#[test]
fn inherited_tmux_captures_relative_paths_and_validates_only_shape() {
    let relative = resolve_server_identity(
        None,
        None,
        inputs(
            Path::new("/work"),
            None,
            Some(OsStr::new("relative/socket,not-a-pid,not-a-session")),
        ),
    )
    .unwrap();

    assert_eq!(relative.socket_path(), Path::new("/work/relative/socket"),);

    let empty_suffixes = resolve_server_identity(
        None,
        None,
        inputs(Path::new("/work"), None, Some(OsStr::new("/sock,,"))),
    )
    .unwrap();
    assert_eq!(empty_suffixes.socket_path(), Path::new("/sock"));
}

#[test]
fn inherited_tmux_preserves_non_utf8_socket_paths() {
    let raw = OsString::from_vec(vec![
        b'/', b't', b'm', b'p', b'/', 0xff, b',', b's', b'o', b'c', b'k', b'e', b't', b',', b'1',
        b',', b'0',
    ]);
    let identity =
        resolve_server_identity(None, None, inputs(Path::new("/work"), None, Some(&raw))).unwrap();

    assert_eq!(
        identity.socket_path().as_os_str().as_bytes(),
        b"/tmp/\xff,socket",
    );
}

#[test]
fn malformed_inherited_tmux_falls_back_to_default() {
    let malformed = [
        None,
        Some(OsStr::new("")),
        Some(OsStr::new(",1,0")),
        Some(OsStr::new("/tmp/socket,1")),
        Some(OsStr::new("/tmp/socket")),
    ];
    let fallback = Path::new("/tmp").canonicalize().unwrap();

    for inherited_tmux in malformed {
        let identity =
            resolve_server_identity(None, None, inputs(Path::new("/work"), None, inherited_tmux))
                .unwrap();
        assert_eq!(identity.socket_path(), fallback.join("tmux-1000/default"),);
    }
}

#[test]
fn named_socket_labels_cannot_escape_the_resolved_root() {
    let invalid = [
        "",
        ".",
        "..",
        "/",
        "/absolute",
        "nested/socket",
        "name/",
        "name//",
        "./name",
        "name/.",
        "nul\0byte",
    ];

    for name in invalid {
        let result = resolve_server_identity(
            None,
            Some(OsStr::new(name)),
            inputs(Path::new("/work"), None, None),
        );
        assert!(result.is_err(), "{name:?} must not escape the socket root");
    }
}

#[test]
fn named_socket_labels_accept_non_utf8_normal_components() {
    let name = OsString::from_vec(vec![b'n', 0xff]);
    let identity =
        resolve_server_identity(None, Some(&name), inputs(Path::new("/work"), None, None)).unwrap();
    let expected = Path::new("/tmp")
        .canonicalize()
        .unwrap()
        .join("tmux-1000")
        .join(&name);

    assert_eq!(identity.socket_path(), expected);
}

#[test]
fn selectors_for_the_same_endpoint_share_server_identity() {
    let root = tempfile::tempdir().unwrap();
    // The explicit selector is compared against ones that resolve through
    // canonicalization, so it has to name the canonical path or they
    // differ wherever the temporary root is itself a symlink.
    let endpoint = root
        .path()
        .canonicalize()
        .unwrap()
        .join("tmux-1000/default");
    let mut inherited_bytes = endpoint.as_os_str().as_bytes().to_vec();
    inherited_bytes.extend_from_slice(b",1,0");
    let inherited = OsString::from_vec(inherited_bytes);
    let context = inputs(
        Path::new("/work"),
        Some(root.path().as_os_str()),
        Some(&inherited),
    );

    let named = resolve_server_identity(None, Some(OsStr::new("default")), context).unwrap();
    let automatic = resolve_server_identity(None, None, context).unwrap();
    let explicit = resolve_server_identity(Some(endpoint.as_os_str()), None, context).unwrap();

    assert_eq!(named, automatic);
    assert_eq!(automatic, explicit);
    assert_eq!(hash(&named), hash(&explicit));
}

#[test]
fn server_identity_debug_does_not_disclose_socket_paths() {
    let identity = resolve_server_identity(
        Some(OsStr::new("/private/SENTINEL/socket")),
        None,
        inputs(Path::new("/work"), None, None),
    )
    .unwrap();

    assert!(!format!("{identity:?}").contains("SENTINEL"));
}

#[test]
fn socket_path_capture_rejects_empty_paths_and_relative_working_directories() {
    let root = PathBuf::from("/tmp");
    let nul_path = OsString::from_vec(vec![b's', b'o', b'c', b'k', b'\0', b'e', b't']);

    assert!(
        resolve_server_identity(
            Some(OsStr::new("")),
            None,
            inputs(Path::new("/work"), Some(root.as_os_str()), None),
        )
        .is_err(),
    );
    assert!(
        resolve_server_identity(
            Some(&nul_path),
            None,
            inputs(Path::new("/work"), Some(root.as_os_str()), None),
        )
        .is_err(),
    );
    assert!(
        resolve_server_identity(
            Some(OsStr::new("socket")),
            None,
            inputs(Path::new("relative"), Some(root.as_os_str()), None),
        )
        .is_err(),
    );
}

#[test]
fn explicit_socket_paths_accept_non_utf8_bytes() {
    let raw = OsString::from_vec(vec![b's', b'o', b'c', b'k', b'e', b't', 0xff]);
    let identity =
        resolve_server_identity(Some(&raw), None, inputs(Path::new("/work"), None, None)).unwrap();

    let mut expected = b"/work/socket".to_vec();
    expected.push(0xff);
    assert_eq!(
        identity.socket_path().as_os_str().as_bytes(),
        expected.as_slice(),
    );
}

fn winlink_hash(value: &WindowLinkIdentity) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

const fn const_winlink_server_identity(identity: &WindowLinkIdentity) -> &ServerIdentity {
    identity.server_identity()
}

const fn const_winlink_session_id(identity: &WindowLinkIdentity) -> &SessionId {
    identity.session_id()
}

const fn const_winlink_window_index(identity: &WindowLinkIdentity) -> i32 {
    identity.window_index()
}

const fn const_winlink_window_id(identity: &WindowLinkIdentity) -> &WindowId {
    identity.window_id()
}

fn winlink_identity_fixture(
    endpoint: &str,
    session_id: &str,
    window_index: i32,
    window_id: &str,
) -> WindowLinkIdentity {
    WindowLinkIdentity::new(
        ServerIdentity::from_socket_path(PathBuf::from(endpoint)),
        session_id.parse().expect("fixture Session ID is valid"),
        window_index,
        window_id.parse().expect("fixture Window ID is valid"),
    )
}

#[test]
fn winlink_identity_constructor_accessors_and_traits_are_exact() {
    let constructor: fn(ServerIdentity, SessionId, i32, WindowId) -> WindowLinkIdentity =
        WindowLinkIdentity::new;
    let server_identity: for<'a> fn(&'a WindowLinkIdentity) -> &'a ServerIdentity =
        WindowLinkIdentity::server_identity;
    let session_id: for<'a> fn(&'a WindowLinkIdentity) -> &'a SessionId =
        WindowLinkIdentity::session_id;
    let window_index: fn(&WindowLinkIdentity) -> i32 = WindowLinkIdentity::window_index;
    let window_id: for<'a> fn(&'a WindowLinkIdentity) -> &'a WindowId =
        WindowLinkIdentity::window_id;
    let identity = constructor(
        ServerIdentity::from_socket_path(PathBuf::from(
            "/private/winlink-constructor-sentinel/socket",
        )),
        "$7".parse().expect("fixture Session ID is valid"),
        -3,
        "@11".parse().expect("fixture Window ID is valid"),
    );

    assert_impl_all!(WindowLinkIdentity: Clone, std::fmt::Debug, Eq, Hash, Send, Sync);
    assert_not_impl_any!(WindowLinkIdentity: Copy);
    let WindowLinkIdentity {
        server_identity: _,
        session_id: _,
        window_index: _,
        window_id: _,
    } = &identity;
    assert_eq!(
        server_identity(&identity),
        &ServerIdentity::from_socket_path(PathBuf::from(
            "/private/winlink-constructor-sentinel/socket",
        )),
    );
    assert_eq!(session_id(&identity).as_ref(), "$7");
    assert_eq!(window_index(&identity), -3);
    assert_eq!(window_id(&identity).as_ref(), "@11");
    assert_eq!(
        const_winlink_server_identity(&identity),
        server_identity(&identity)
    );
    assert_eq!(const_winlink_session_id(&identity), session_id(&identity));
    assert_eq!(
        const_winlink_window_index(&identity),
        window_index(&identity)
    );
    assert_eq!(const_winlink_window_id(&identity), window_id(&identity));
}

#[test]
fn winlink_identity_equality_and_hash_use_all_four_components() {
    let base = winlink_identity_fixture("/private/endpoint-a", "$1", -2, "@3");
    let variants = [
        winlink_identity_fixture("/private/endpoint-b", "$1", -2, "@3"),
        winlink_identity_fixture("/private/endpoint-a", "$2", -2, "@3"),
        winlink_identity_fixture("/private/endpoint-a", "$1", 4, "@3"),
        winlink_identity_fixture("/private/endpoint-a", "$1", -2, "@4"),
    ];

    assert_eq!(base, base.clone());
    for variant in &variants {
        assert_ne!(&base, variant);
        assert_ne!(winlink_hash(&base), winlink_hash(variant));
    }
}

#[test]
fn winlink_identity_debug_redacts_the_endpoint() {
    let identity = winlink_identity_fixture(
        "/private/winlink-debug-endpoint-sentinel/socket",
        "$13",
        17,
        "@19",
    );
    let debug = format!("{identity:?}");

    assert!(debug.contains("WindowLinkIdentity"));
    assert!(!debug.contains("winlink-debug-endpoint-sentinel"));
    assert!(!debug.contains("/private"));
}
