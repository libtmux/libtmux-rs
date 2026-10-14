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
    socket_root: Option<&'a OsStr>,
    inherited_tmux: Option<&'a OsStr>,
) -> EndpointInputs<'a> {
    EndpointInputs::new(socket_root, 1000, inherited_tmux)
}

#[test]
fn absolute_socket_paths_define_identity_without_normalising_bytes() {
    let left = resolve_server_identity(
        Some(OsStr::new("/tmp/a/../socket")),
        None,
        inputs(None, None),
    )
    .unwrap();
    let same = resolve_server_identity(
        Some(OsStr::new("/tmp/a/../socket")),
        None,
        inputs(None, None),
    )
    .unwrap();
    let other =
        resolve_server_identity(Some(OsStr::new("/tmp/socket")), None, inputs(None, None)).unwrap();
    assert_eq!(left, same);
    assert_eq!(hash(&left), hash(&same));
    assert_ne!(left, other);
    for path in ["", "relative/socket", "nul\0byte"] {
        assert!(resolve_server_identity(Some(OsStr::new(path)), None, inputs(None, None)).is_err());
    }
    let raw = OsString::from_vec(b"/tmp/socket\xff".to_vec());
    let identity = resolve_server_identity(Some(&raw), None, inputs(None, None)).unwrap();
    assert_eq!(identity.socket_path().as_os_str(), raw);
}

#[test]
fn explicit_socket_identity_preserves_raw_separator_spelling() {
    let plain =
        resolve_server_identity(Some(OsStr::new("/tmp/socket")), None, inputs(None, None)).unwrap();
    for path in ["/tmp/socket/", "//tmp/socket"] {
        let other =
            resolve_server_identity(Some(OsStr::new(path)), None, inputs(None, None)).unwrap();
        assert_ne!(plain, other);
    }
}

#[test]
fn default_resolution_needs_no_daemon_and_uses_the_real_uid_directory() {
    for value in [None, Some(OsStr::new(""))] {
        let context = inputs(value, value).with_defaults(value, value);
        let identity = resolve_server_identity(None, None, context).unwrap();
        assert_eq!(
            identity.socket_path(),
            Path::new("/tmp")
                .canonicalize()
                .unwrap()
                .join("tmux-1000/default")
        );
    }
}

#[test]
fn selectors_have_one_precedence_and_ignore_lower_invalid_values() {
    let root = tempfile::tempdir().unwrap();
    let bad = Some(OsStr::new("malformed"));
    let context = inputs(Some(root.path().as_os_str()), bad)
        .with_defaults(Some(OsStr::new("/env/path")), Some(OsStr::new("bad/name")));
    let explicit =
        resolve_server_identity(Some(OsStr::new("/explicit/path")), None, context).unwrap();
    assert_eq!(explicit.socket_path(), Path::new("/explicit/path"));
    let named = resolve_server_identity(None, Some(OsStr::new("explicit-name")), context).unwrap();
    assert_eq!(
        named.socket_path(),
        root.path()
            .canonicalize()
            .unwrap()
            .join("tmux-1000/explicit-name")
    );
    let path = resolve_server_identity(None, None, context).unwrap();
    assert_eq!(path.socket_path(), Path::new("/env/path"));
    let env_name = resolve_server_identity(
        None,
        None,
        inputs(Some(root.path().as_os_str()), bad)
            .with_defaults(Some(OsStr::new("")), Some(OsStr::new("env-name"))),
    )
    .unwrap();
    assert_eq!(
        env_name.socket_path(),
        root.path()
            .canonicalize()
            .unwrap()
            .join("tmux-1000/env-name")
    );
    assert!(
        resolve_server_identity(
            Some(OsStr::new("/explicit/path")),
            Some(OsStr::new("name")),
            context
        )
        .is_err()
    );
}

#[test]
fn invalid_selected_defaults_do_not_fall_through() {
    let root = tempfile::tempdir().unwrap();
    let context = inputs(
        Some(root.path().as_os_str()),
        Some(OsStr::new("/valid/socket,12,0")),
    );
    for path in ["relative", "nul\0byte"] {
        assert!(
            resolve_server_identity(
                None,
                None,
                context.with_defaults(Some(OsStr::new(path)), Some(OsStr::new("valid")))
            )
            .is_err()
        );
    }
    for name in [".", "..", "nested/name", "/absolute", "nul\0byte"] {
        assert!(
            resolve_server_identity(
                None,
                None,
                context.with_defaults(None, Some(OsStr::new(name)))
            )
            .is_err()
        );
    }
    assert!(resolve_server_identity(None, Some(OsStr::new("")), context).is_err());
}

#[test]
fn named_sockets_preserve_spaces_and_non_utf8_components() {
    for name in [
        OsString::from(" name "),
        OsString::from_vec(b"n\xff".to_vec()),
    ] {
        let result = resolve_server_identity(None, Some(&name), inputs(None, None)).unwrap();
        assert_eq!(
            result.socket_path(),
            Path::new("/tmp")
                .canonicalize()
                .unwrap()
                .join("tmux-1000")
                .join(name)
        );
    }
}

#[test]
fn selected_root_is_absolute_existing_and_captured_through_symlinks() {
    let workspace = tempfile::tempdir().unwrap();
    let actual = workspace.path().join(" actual ");
    std::fs::create_dir(&actual).unwrap();
    let link = workspace.path().join("link");
    symlink(&actual, &link).unwrap();
    let identity =
        resolve_server_identity(None, None, inputs(Some(link.as_os_str()), None)).unwrap();
    std::fs::remove_file(&link).unwrap();
    symlink("/different/root", &link).unwrap();
    assert_eq!(
        identity.socket_path(),
        actual.canonicalize().unwrap().join("tmux-1000/default")
    );
    for root in [
        Path::new("relative"),
        workspace.path().join("missing").as_path(),
    ] {
        assert!(resolve_server_identity(None, None, inputs(Some(root.as_os_str()), None)).is_err());
    }
    let ignored = resolve_server_identity(
        Some(OsStr::new("/socket")),
        None,
        inputs(Some(OsStr::new("relative")), None),
    )
    .unwrap();
    assert_eq!(ignored.socket_path(), Path::new("/socket"));
}

#[test]
fn inherited_tmux_is_split_from_the_right_and_validates_the_triple() {
    for value in [
        "/tmp/od,d/socket,84215,3",
        "/tmp/od,d/socket,84215,$3",
        "/tmp/od,d/socket,84215,-1",
    ] {
        let identity =
            resolve_server_identity(None, None, inputs(None, Some(OsStr::new(value)))).unwrap();
        assert_eq!(identity.socket_path(), Path::new("/tmp/od,d/socket"));
    }
    for value in [
        ",1,0",
        "/tmp/socket",
        "/tmp/socket,1",
        "/tmp/socket,,",
        "/tmp/socket,0,1",
        "/tmp/socket,-1,0",
        "/tmp/socket,1,-2",
        "/tmp/socket,1,$-1",
        "/tmp/socket,1,$$0",
        "relative,1,0",
        "/tmp/socket,１２,0",
    ] {
        assert!(
            resolve_server_identity(None, None, inputs(None, Some(OsStr::new(value)))).is_err(),
            "{value:?}"
        );
    }
    let raw = OsString::from_vec(b"/tmp/\xff,socket,1,0".to_vec());
    let identity = resolve_server_identity(None, None, inputs(None, Some(&raw))).unwrap();
    assert_eq!(
        identity.socket_path().as_os_str().as_bytes(),
        b"/tmp/\xff,socket"
    );
}

#[test]
fn selectors_for_the_same_endpoint_share_server_identity() {
    let root = tempfile::tempdir().unwrap();
    let endpoint = root
        .path()
        .canonicalize()
        .unwrap()
        .join("tmux-1000/default");
    let mut inherited_bytes = endpoint.as_os_str().as_bytes().to_vec();
    inherited_bytes.extend_from_slice(b",1,0");
    let inherited = OsString::from_vec(inherited_bytes);
    let context = inputs(Some(root.path().as_os_str()), Some(&inherited));
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
        inputs(None, None),
    )
    .unwrap();
    assert!(!format!("{identity:?}").contains("SENTINEL"));
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
