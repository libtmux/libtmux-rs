use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::process::ChildStdin;

pub(super) use libtmux::protocol::Line;
#[cfg(test)]
pub(super) use libtmux::protocol::unescape_output;

use crate::Error;

/// Read and classify one protocol line.
///
/// `pending` carries a line across calls. `read_until` appends what it read
/// before it was cancelled, which is what makes this usable in `select!` --
/// `read_line` would lose those bytes, and would also reject the pane output
/// that is not UTF-8.
pub(super) async fn read_line<R: AsyncBufRead + Unpin + ?Sized>(
    stdout: &mut R,
    pending: &mut Vec<u8>,
    limit: usize,
) -> Result<Option<Line>, Error> {
    read_line_within(stdout, pending, limit, None).await
}

/// Read one line, classifying it for the block it arrived in.
///
/// `within` names the open block, if any. tmux(1) guarantees a notification
/// never appears inside an output block, so every line but its own terminator
/// is command output -- including one that looks like a notification, which
/// is what `list-panes -F '#{pane_id}'` produces for every row.
pub(super) async fn read_line_within<R: AsyncBufRead + Unpin + ?Sized>(
    stdout: &mut R,
    pending: &mut Vec<u8>,
    limit: usize,
    within: Option<u64>,
) -> Result<Option<Line>, Error> {
    // One byte past the budget is enough to know the line broke it, and
    // capping the read is what keeps the answer bounded: `read_until` on its
    // own appends until it finds a newline, so checking the length afterwards
    // measures memory already taken. `pending` carries bytes left by a
    // cancelled read, so the cap is what is left of the budget, not all of it.
    let allowance = limit.saturating_sub(pending.len()).saturating_add(1);
    let read = {
        let mut bounded = tokio::io::AsyncReadExt::take(&mut *stdout, allowance as u64);
        bounded
            .read_until(b'\n', pending)
            .await
            .map_err(Error::control_mode)?
    };
    if read == 0 && pending.is_empty() {
        return Ok(None);
    }
    // A line that never ends is the one shape a framed protocol cannot
    // recover from by reading further, so it stops here rather than growing.
    // The connection is not resynchronizable afterwards: the caller reopens.
    if pending.len() > limit {
        pending.clear();
        return Err(Error::control_mode_frame_too_large("line", limit));
    }

    // read_until stops at the newline or at end of input, so what is left
    // without one is the last line tmux managed to write.
    let bytes = pending.strip_suffix(b"\n").unwrap_or(pending);
    let line = match within {
        Some(number) => Line::parse_within_block(bytes, number),
        None => Line::parse(bytes),
    };
    pending.clear();

    Ok(Some(line))
}

/// Write a command and its independent completion boundary in one flush.
pub(super) async fn write_request(
    stdin: &mut ChildStdin,
    line: &str,
    fence: &str,
) -> Result<(), Error> {
    for part in [line.as_bytes(), b"\n", fence.as_bytes(), b"\n"] {
        stdin.write_all(part).await.map_err(Error::control_mode)?;
    }
    stdin.flush().await.map_err(Error::control_mode)
}
