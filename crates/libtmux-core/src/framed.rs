//! Decode only the initial blocks of a one-shot, non-attaching control client.
use crate::{Error, parse_number};

pub(super) struct Frames<'a>(&'a [u8]);

/// A lost client may have delivered its creation block before the terminal %exit.
pub(super) fn collected_prefix(bytes: &[u8]) -> Frames<'_> {
    Frames(bytes)
}

pub(super) fn first(reply: &[u8]) -> Result<Frames<'_>, Error> {
    reply
        .strip_suffix(b"%exit\n")
        .map(Frames)
        .ok_or(Error::FramedRead)
}

impl Frames<'_> {
    pub(super) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<'a> Frames<'a> {
    /// A cold daemon can report a configuration diagnostic before the command.
    pub(super) fn skip_creation_notices(mut self) -> Self {
        while let Some((line, rest)) = split_record(self.0) {
            if !creation_notice(line) {
                break;
            }
            self.0 = rest;
        }
        self
    }

    /// Check hook blocks once, discarding their payloads. The only receipt comes
    /// from the independently verified first action block.
    pub(super) fn followups_succeeded(self, mut previous: u64) -> bool {
        let mut active = None;
        for line in self.0.split_inclusive(|&b| b == b'\n') {
            if let Some(header) = active {
                if line.strip_prefix(b"%error ") == Some(header) {
                    return false;
                }
                if line.strip_prefix(b"%end ") == Some(header) {
                    active = None;
                }
            } else if !creation_notice(line) {
                let Some(header) = line.strip_prefix(b"%begin ") else {
                    return false;
                };
                let Ok(number) = command_number(header) else {
                    return false;
                };
                if number <= previous {
                    return false;
                }
                previous = number;
                active = Some(header);
            }
        }
        active.is_none()
    }

    pub(super) fn take(self, previous: Option<u64>) -> Result<(Block<'a>, Self), Error> {
        let mut records = self.0.split_inclusive(|&byte| byte == b'\n');
        let start = records.next().ok_or(Error::FramedRead)?;
        let header = start.strip_prefix(b"%begin ").ok_or(Error::FramedRead)?;
        let number = command_number(header)?;
        if previous.is_some_and(|previous| number <= previous) {
            return Err(Error::FramedRead);
        }

        let mut offset = start.len();
        let mut matching_end = None;
        for record in records {
            let success = record.strip_prefix(b"%end ") == Some(header);
            let error = record.strip_prefix(b"%error ") == Some(header);
            if (success || error)
                && matching_end
                    .replace((offset, offset + record.len(), success))
                    .is_some()
            {
                // A screen or later hook can print a record with this same tuple.
                return Err(Error::FramedRead);
            }
            offset += record.len();
        }
        let (end, next, success) = matching_end.ok_or(Error::FramedRead)?;
        let block = Block {
            number,
            success,
            bytes: &self.0[start.len()..end],
        };
        Ok((block, Self(&self.0[next..])))
    }
}

fn split_record(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let end = bytes.iter().position(|&b| b == b'\n')? + 1;
    Some(bytes.split_at(end))
}

fn creation_notice(line: &[u8]) -> bool {
    line == b"%sessions-changed\n"
        || line.starts_with(b"%config-error ")
        || line.starts_with(b"%pane-mode-changed ")
}

fn command_number(header: &[u8]) -> Result<u64, Error> {
    let tuple = header.strip_suffix(b"\n").ok_or(Error::FramedRead)?;
    let text = std::str::from_utf8(tuple).map_err(|_| Error::FramedRead)?;
    let mut fields = text.split(' ');
    parse_number::<u64>(fields.next().ok_or(Error::FramedRead)?).ok_or(Error::FramedRead)?;
    let number = parse_number(fields.next().ok_or(Error::FramedRead)?).ok_or(Error::FramedRead)?;
    // One-shot command frames report zero rather than the flag used for commands read on stdin.
    if fields.next() != Some("0") || fields.next().is_some() {
        return Err(Error::FramedRead);
    }
    Ok(number)
}

pub(super) struct Block<'a> {
    pub(super) number: u64,
    pub(super) success: bool,
    pub(super) bytes: &'a [u8],
}

impl<'a> Block<'a> {
    pub(super) fn success_bytes(&self) -> Result<&'a [u8], Error> {
        self.success.then_some(self.bytes).ok_or(Error::FramedRead)
    }
}
