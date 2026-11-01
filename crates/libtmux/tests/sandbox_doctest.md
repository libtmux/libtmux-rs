The tmux sandbox, checked from inside a doctest.

`cargo test --doc` runs every doctest as a process of its own, so the sandbox
reaches it only through the environment it inherits: the runner on the pinned
toolchain, `[env]` in `.cargo/config.toml` on an older one. This block asserts
what it inherited and then starts a bare tmux client to see where the server
lands. It names a variable when one is wrong and never prints a value.
`scripts/check-doctest-sandbox.sh` says through `LIBTMUX_DOCTEST_EXPECTS`
which way in it should have come, and runs it again with the sandbox broken
in turn, where it must fail each time.

```rust
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn main() {
    // The runner unsets these and `[env]` empties them; either is no server.
    for name in ["TMUX", "TMUX_PANE", "LIBTMUX_SOCKET", "LIBTMUX_SOCKET_PATH"] {
        assert!(
            std::env::var_os(name).is_none_or(|value| value.is_empty()),
            "{name} is set; this doctest is not in the tmux sandbox",
        );
    }

    let tmpdir =
        std::env::var_os("TMUX_TMPDIR").filter(|value| !value.is_empty());
    let marker = std::env::var_os("LIBTMUX_TEST_SANDBOX")
        .filter(|value| !value.is_empty());
    assert!(
        tmpdir.is_some() && tmpdir == marker,
        "TMUX_TMPDIR is not LIBTMUX_TEST_SANDBOX; the sandbox did not apply",
    );
    let sandbox = PathBuf::from(marker.expect("checked above"));
    assert!(
        sandbox.is_absolute(),
        "LIBTMUX_TEST_SANDBOX is not absolute"
    );
    let metadata =
        std::fs::metadata(&sandbox).expect("LIBTMUX_TEST_SANDBOX exists");
    let uid = rustix::process::getuid().as_raw();
    assert!(metadata.is_dir(), "LIBTMUX_TEST_SANDBOX is not a directory");
    assert!(
        metadata.uid() == uid,
        "LIBTMUX_TEST_SANDBOX is another user's"
    );
    assert!(
        metadata.mode() & 0o002 == 0,
        "LIBTMUX_TEST_SANDBOX is world-writable",
    );

    // sun_path holds 104 bytes on macOS and 108 on Linux.
    let socket = sandbox.join(format!("tmux-{uid}")).join("default");
    assert!(
        socket.as_os_str().len() <= 100,
        "the default socket path under LIBTMUX_TEST_SANDBOX is over 100 \
         bytes; use a shorter checkout path or TMPDIR",
    );
    // Only the check script sets this, to say which way in it expects.
    let fallback = sandbox.ends_with(".cargo/tmux-sandbox");
    let unset = std::env::var_os("TMUX").is_none();
    match std::env::var("LIBTMUX_DOCTEST_EXPECTS").as_deref() {
        Ok("runner") => {
            assert!(
                unset && !fallback,
                "the runner did not reach this doctest"
            );
        }
        Ok("env") => assert!(
            !unset && fallback,
            "`[env]` and not the runner should have reached this doctest",
        ),
        _ => {}
    }

    assert!(
        UnixStream::connect(&socket).is_err(),
        "something listens on the sandbox's default socket",
    );

    // A bare client, with no -L and no -S, inheriting this process's
    // environment unchanged. Everything above ran first, so a client that
    // would have followed a variable to someone's server never starts.
    let tmux =
        std::env::var_os("LIBTMUX_TEST_TMUX").unwrap_or_else(|| "tmux".into());
    let quietly = |arguments: &[&str]| {
        Command::new(&tmux)
            .args(arguments)
            .env("SHELL", "/bin/sh")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    if !quietly(&["-V"]) {
        eprintln!("tmux is not on PATH; no bare client to run");
        return;
    }
    assert!(
        quietly(&["-f", "/dev/null", "new-session", "-d"]),
        "a bare tmux client did not start a server",
    );
    let landed = UnixStream::connect(&socket).is_ok();
    let path = socket.to_str().expect("UTF-8 socket path");
    quietly(&["-S", path, "kill-server"]);
    for _ in 0..50 {
        if UnixStream::connect(&socket).is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        landed,
        "a bare tmux client missed the sandbox's default socket",
    );
}
```
