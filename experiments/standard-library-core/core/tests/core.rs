//! Public core contracts used by the standalone consumers.

use libtmux::{CapturePane, PaneId, TmuxText, decode_panes};

#[test]
fn pane_ids_are_typed_and_normalised() {
    let pane: PaneId = "%003".parse().unwrap();
    assert_eq!(pane.to_string(), "%3");
    assert!(pane < "%10".parse().unwrap());
    for invalid in ["@3", "%", "%+3", "%4294967296", "%3;kill-server"] {
        assert!(invalid.parse::<PaneId>().is_err(), "accepted {invalid:?}");
    }
}

#[test]
fn detached_snapshots_build_capture_requests() {
    let panes = decode_panes(b"%0\t80\t24\n%1\t120\t40\n").unwrap();
    assert_eq!(panes.len(), 2);
    assert_eq!((panes[1].width, panes[1].height), (120, 40));
    let request = CapturePane::new(panes[1].id.clone()).history(25);
    assert_eq!(
        request.args(),
        ["capture-pane", "-p", "-t", "%1", "-S", "-25"]
    );
    assert!(decode_panes(b"").unwrap().is_empty());
    assert_eq!(
        decode_panes(b"%0\t80\t24\ninvalid\n").unwrap_err().line(),
        2
    );
    assert!(decode_panes(b"%0\t80\t24\textra\n").is_err());
    assert!(decode_panes(b"%0\t65536\t24\n").is_err());
    assert!(decode_panes(b"%0\t80\t\xff\n").is_err());
}

#[test]
fn capture_retains_non_utf8_bytes() {
    let text = TmuxText::from_bytes([0, 255, b'\n']);
    assert_eq!(text.as_bytes(), [0, 255, b'\n']);
    assert!(text.as_str().is_err());
    assert_eq!(text.to_string_lossy(), "\0\u{fffd}\n");
}
