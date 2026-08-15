//! Turning a pane's byte stream into text a pattern can be matched against.
//!
//! A pane writes for a terminal, not for a reader: the bytes carry cursor
//! movement, colour, and repaint alongside the characters. Matching against
//! them raw finds a pattern only when nothing coloured it.
//!
//! This is not a terminal. It removes the sequences that would otherwise sit
//! between the characters of a word, and it turns carriage returns into line
//! breaks so a line rewritten in place reads as a later line rather than
//! running into its predecessor. What it cannot do is resolve cursor
//! addressing: a program that draws by moving the cursor produces text here in
//! the order it was written, not in the order it appears on screen.

/// Where the escape-sequence scanner is between chunks.
///
/// A sequence can be split across whatever tmux chose to report at once, so
/// the scanner's position outlives a single chunk.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum State {
    /// Ordinary text.
    #[default]
    Text,
    /// Just past `ESC`, waiting to learn which kind of sequence this is.
    Escape,
    /// Inside `ESC [ ... final`, which ends at a byte in `0x40..=0x7e`.
    ControlSequence,
    /// Inside a string sequence (`OSC`, `APC`, `PM`, `DCS`), which ends at
    /// `BEL` or at `ESC \`.
    String,
    /// Inside a string sequence, just past an `ESC` that may terminate it.
    StringEscape,
}

/// Strips escape sequences from a pane's output, across chunk boundaries.
#[derive(Debug, Default)]
pub(crate) struct TextFilter {
    state: State,
    /// Whether the last byte written was a carriage return, which decides
    /// whether the next newline is a fresh line or the same one.
    pending_return: bool,
}

impl TextFilter {
    /// Start a filter at the beginning of a stream.
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            state: State::Text,
            pending_return: false,
        }
    }

    /// Append the readable text of one chunk to `out`.
    pub(crate) fn push(&mut self, chunk: &[u8], out: &mut Vec<u8>) {
        for &byte in chunk {
            self.push_byte(byte, out);
        }
    }

    fn push_byte(&mut self, byte: u8, out: &mut Vec<u8>) {
        match self.state {
            State::Text => self.push_text_byte(byte, out),
            State::Escape => {
                self.state = match byte {
                    b'[' => State::ControlSequence,
                    // OSC, APC, PM, DCS all run until a string terminator.
                    b']' | b'_' | b'^' | b'P' => State::String,
                    // Everything else is a two-byte sequence, already whole.
                    _ => State::Text,
                };
            }
            State::ControlSequence => {
                if (0x40..=0x7e).contains(&byte) {
                    self.state = State::Text;
                }
            }
            State::String => match byte {
                0x07 => self.state = State::Text,
                0x1b => self.state = State::StringEscape,
                _ => {}
            },
            State::StringEscape => {
                // `ESC \` ends the string; any other ESC-something restarts
                // the wait for a terminator rather than ending it.
                self.state = if byte == b'\\' {
                    State::Text
                } else {
                    State::String
                };
            }
        }
    }

    fn push_text_byte(&mut self, byte: u8, out: &mut Vec<u8>) {
        match byte {
            0x1b => self.state = State::Escape,
            b'\r' => {
                // Held: `\r\n` is one line break, and a lone `\r` is a line
                // rewritten in place, which reads better as another line than
                // as text running into what it replaced.
                self.pending_return = true;
            }
            b'\n' => {
                self.pending_return = false;
                out.push(b'\n');
            }
            // A backspace is how a shell erases; dropping the erased byte
            // keeps a re-edited command line from reading as both versions.
            0x08 => {
                self.flush_return(out);
                if out.last().is_some_and(|&last| last != b'\n') {
                    out.pop();
                }
            }
            _ => {
                self.flush_return(out);
                out.push(byte);
            }
        }
    }

    fn flush_return(&mut self, out: &mut Vec<u8>) {
        if std::mem::take(&mut self.pending_return) {
            out.push(b'\n');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(chunks: &[&[u8]]) -> String {
        let mut filter = TextFilter::new();
        let mut out = Vec::new();
        for chunk in chunks {
            filter.push(chunk, &mut out);
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn plain_text_survives() {
        assert_eq!(filtered(&[b"hello"]), "hello");
    }

    #[test]
    fn colour_is_removed_from_the_middle_of_a_word() {
        assert_eq!(filtered(&[b"he\x1b[1;32mll\x1b[0mo"]), "hello");
    }

    #[test]
    fn a_sequence_split_across_chunks_is_still_removed() {
        assert_eq!(filtered(&[b"he\x1b[1;3", b"2mllo"]), "hello");
    }

    #[test]
    fn an_escape_alone_at_the_end_of_a_chunk_is_removed() {
        assert_eq!(filtered(&[b"he\x1b", b"[0mllo"]), "hello");
    }

    #[test]
    fn an_operating_system_command_is_removed_at_bel() {
        assert_eq!(filtered(&[b"a\x1b]0;title\x07b"]), "ab");
    }

    #[test]
    fn an_application_program_command_is_removed_at_string_terminator() {
        assert_eq!(filtered(&[b"a\x1b_marker\x1b\\b"]), "ab");
    }

    #[test]
    fn an_escape_inside_a_string_does_not_end_it_early() {
        assert_eq!(filtered(&[b"a\x1b]0;ti\x1b(tle\x07b"]), "ab");
    }

    #[test]
    fn a_two_byte_sequence_is_removed_whole() {
        assert_eq!(filtered(&[b"a\x1b=b\x1b>c"]), "abc");
    }

    #[test]
    fn carriage_return_and_newline_are_one_break() {
        assert_eq!(filtered(&[b"one\r\ntwo\r\n"]), "one\ntwo\n");
    }

    #[test]
    fn a_lone_carriage_return_starts_a_line() {
        assert_eq!(filtered(&[b"50%\r100%"]), "50%\n100%");
    }

    #[test]
    fn a_trailing_carriage_return_does_not_emit_until_something_follows() {
        assert_eq!(filtered(&[b"done\r"]), "done");
    }

    #[test]
    fn backspace_erases_the_previous_character() {
        assert_eq!(filtered(&[b"cat\x08p"]), "cap");
    }

    #[test]
    fn backspace_does_not_eat_a_line_break() {
        assert_eq!(filtered(&[b"a\n\x08b"]), "a\nb");
    }

    #[test]
    fn utf8_passes_through_bytewise() {
        assert_eq!(filtered(&["héllo".as_bytes()]), "héllo");
    }
}
