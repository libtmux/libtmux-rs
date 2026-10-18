use std::io::{self, BufRead, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::time::Instant;

pub(super) use crate::protocol::Line;
#[cfg(test)]
pub(super) use crate::protocol::unescape_output;

use crate::Error;

/// One bounded physical line. Interpretation belongs to the actor's block state.
#[derive(Debug)]
pub(super) struct RawLine {
    pub(super) bytes: Vec<u8>,
    pub(super) wire_bytes: usize,
}

pub(super) fn read_raw<R: BufRead + ?Sized>(
    stdout: &mut R,
    limit: usize,
) -> Result<Option<RawLine>, Error> {
    let mut bytes = Vec::new();
    let read = stdout
        .take(limit.saturating_add(1) as u64)
        .read_until(b'\n', &mut bytes)
        .map_err(Error::control_mode)?;
    if read == 0 {
        return Ok(None);
    }
    if bytes.len() > limit {
        return Err(Error::control_mode_frame_too_large("line", limit));
    }
    let wire_bytes = bytes.len();
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    Ok(Some(RawLine { bytes, wire_bytes }))
}

#[cfg(feature = "unstable-fuzzing")]
pub(super) fn read_line_within<R: BufRead + ?Sized>(
    stdout: &mut R,
    limit: usize,
    within: Option<u64>,
) -> Result<Option<Line>, Error> {
    Ok(read_raw(stdout, limit)?.map(|line| match within {
        Some(number) => Line::parse_within_block(&line.bytes, number),
        None => Line::parse(&line.bytes),
    }))
}

/// Preserve one deadline across every partial write of both command lines.
pub(super) fn write_request(
    stdin: &mut UnixStream,
    line: &str,
    fence: &str,
    deadline: Option<Instant>,
) -> Result<(), Error> {
    for mut part in [line.as_bytes(), b"\n", fence.as_bytes(), b"\n"] {
        while !part.is_empty() {
            let remaining = match deadline {
                Some(deadline) => Some(
                    deadline
                        .checked_duration_since(Instant::now())
                        .filter(|value| !value.is_zero())
                        .ok_or_else(Error::control_mode_timeout)?,
                ),
                None => None,
            };
            stdin
                .set_write_timeout(remaining)
                .map_err(Error::control_mode)?;
            match stdin.write(part) {
                Ok(0) => return Err(Error::control_mode_closed()),
                Ok(written) => part = &part[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Err(Error::control_mode_timeout());
                }
                Err(error) => return Err(Error::control_mode(error)),
            }
        }
    }
    Ok(())
}
