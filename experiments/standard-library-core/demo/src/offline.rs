//! Construct and decode tmux data without linking a runtime.

use libtmux::{CapturePane, TmuxText, decode_panes};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let panes = decode_panes(b"%003\t80\t24\n")?;
    let request = CapturePane::new(panes[0].id.clone()).history(20);
    println!(
        "pane {}: {} x {}",
        panes[0].id, panes[0].width, panes[0].height
    );
    println!("argv: {:?}", request.args());
    let output = TmuxText::from_bytes(b"hello\xff\n".to_vec());
    println!("capture bytes: {:?}", output.as_bytes());
    Ok(())
}
