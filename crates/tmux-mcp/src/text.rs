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
//!
//! Where a sequence ends is tmux's call: text swallowed here that tmux showed
//! is output a caller never sees. So each state is one of the transition
//! tables in tmux's `input.c`, named on the variant, merged only where the
//! tables agree on what a reader sees. tmux also abandons a string after five
//! seconds without a terminator (`input_start_ground_timer`); this has no
//! clock, so a string nothing ends hides text until the next `ESC`.

/// `ESC`, which starts a sequence from every state but device control data.
const ESCAPE: u8 = 0x1b;
/// CAN and SUB, which abandon a sequence wherever `ESC` would start one.
const CANCEL: u8 = 0x18;
const SUBSTITUTE: u8 = 0x1a;
/// BEL, which ends an operating system command and no other string.
const BELL: u8 = 0x07;

/// Where the escape-sequence scanner is between chunks.
///
/// A sequence can be split across whatever tmux chose to report at once, so
/// the scanner's position outlives a single chunk.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum State {
    /// `ground`: ordinary text.
    #[default]
    Text,
    /// `esc_enter`: just past `ESC`.
    Escape,
    /// `esc_intermediate`: `ESC` then bytes in `0x20..=0x2f`, as in `ESC ( B`.
    EscapeIntermediate,
    /// `csi_enter`, `csi_parameter`, `csi_intermediate` and `csi_ignore`.
    ControlSequence,
    /// `dcs_enter`: just past `ESC P`.
    DeviceControlEnter,
    /// `dcs_parameter`.
    DeviceControlParameter,
    /// `dcs_intermediate`.
    DeviceControlIntermediate,
    /// `dcs_handler`: device control data, which only `ESC \` ends. The data
    /// carries escapes of its own, as tmux's passthrough does.
    DeviceControl,
    /// `dcs_escape`: device control data just past an `ESC`.
    DeviceControlEscape,
    /// `osc_string`, which `BEL` also ends. C1 ST (`0x9c`) is data to tmux.
    OperatingSystemCommand,
    /// `apc_string`, `rename_string`, `consume_st` and `dcs_ignore`.
    String,
}

/// Strips escape sequences from a pane's output, across chunk boundaries.
#[derive(Clone, Debug, Default)]
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
        let mut out = Some(out);
        for &byte in chunk {
            self.push_byte(byte, &mut out);
        }
    }

    /// Advance the scanner without retaining rendered text.
    pub(crate) fn advance(&mut self, chunk: &[u8]) {
        let mut out = None;
        for &byte in chunk {
            self.push_byte(byte, &mut out);
        }
    }

    fn push_byte(&mut self, byte: u8, out: &mut Option<&mut Vec<u8>>) {
        // `INPUT_STATE_ANYWHERE`, in every table but the two holding device
        // control data.
        if !matches!(
            self.state,
            State::DeviceControl | State::DeviceControlEscape
        ) {
            match byte {
                CANCEL | SUBSTITUTE => {
                    self.state = State::Text;
                    return;
                }
                ESCAPE => {
                    self.state = State::Escape;
                    return;
                }
                _ => {}
            }
        }

        // A byte an arm does not name is collected or ignored in place.
        self.state = match self.state {
            State::Text => self.execute(byte, out, State::Text),
            State::Escape => match byte {
                0x00..=0x1f => self.execute(byte, out, State::Escape),
                0x20..=0x2f => State::EscapeIntermediate,
                b'P' => State::DeviceControlEnter,
                b'[' => State::ControlSequence,
                b']' => State::OperatingSystemCommand,
                // SOS, PM, APC, and screen's `ESC k` title.
                b'X' | b'^' | b'_' | b'k' => State::String,
                // Any other final byte completes a two-byte sequence.
                0x30..=0x7e => State::Text,
                _ => State::Escape,
            },
            State::EscapeIntermediate => match byte {
                0x00..=0x1f => self.execute(byte, out, State::EscapeIntermediate),
                0x30..=0x7e => State::Text,
                _ => State::EscapeIntermediate,
            },
            State::ControlSequence => match byte {
                0x00..=0x1f => self.execute(byte, out, State::ControlSequence),
                0x40..=0x7e => State::Text,
                _ => State::ControlSequence,
            },
            State::DeviceControlEnter => match byte {
                0x20..=0x2f => State::DeviceControlIntermediate,
                0x3a => State::String,
                0x30..=0x3f => State::DeviceControlParameter,
                0x40..=0x7e => State::DeviceControl,
                _ => State::DeviceControlEnter,
            },
            State::DeviceControlParameter => match byte {
                0x20..=0x2f => State::DeviceControlIntermediate,
                0x3a | 0x3c..=0x3f => State::String,
                0x40..=0x7e => State::DeviceControl,
                _ => State::DeviceControlParameter,
            },
            State::DeviceControlIntermediate => match byte {
                0x30..=0x3f => State::String,
                0x40..=0x7e => State::DeviceControl,
                _ => State::DeviceControlIntermediate,
            },
            State::DeviceControl if byte == ESCAPE => State::DeviceControlEscape,
            State::DeviceControlEscape if byte == b'\\' => State::Text,
            State::DeviceControlEscape => State::DeviceControl,
            State::OperatingSystemCommand if byte == BELL => State::Text,
            state @ (State::DeviceControl | State::OperatingSystemCommand | State::String) => state,
        };
    }

    /// Act on `byte` as text would, then stay in `state`: tmux runs a control
    /// byte inside an escape or control sequence and carries on around it.
    fn execute(&mut self, byte: u8, out: &mut Option<&mut Vec<u8>>, state: State) -> State {
        self.push_text_byte(byte, out);
        state
    }

    fn push_text_byte(&mut self, byte: u8, out: &mut Option<&mut Vec<u8>>) {
        match byte {
            b'\r' => {
                // Held: `\r\n` is one line break, and a lone `\r` is a line
                // rewritten in place, which reads better as another line than
                // as text running into what it replaced.
                self.pending_return = true;
            }
            b'\n' => {
                self.pending_return = false;
                if let Some(out) = out.as_deref_mut() {
                    out.push(b'\n');
                }
            }
            // A backspace is how a shell erases; dropping the erased byte
            // keeps a re-edited command line from reading as both versions.
            0x08 => {
                self.flush_return(out);
                if let Some(out) = out.as_deref_mut()
                    && out.last().is_some_and(|&last| last != b'\n')
                {
                    out.pop();
                }
            }
            _ => {
                self.flush_return(out);
                if let Some(out) = out.as_deref_mut() {
                    out.push(byte);
                }
            }
        }
    }

    fn flush_return(&mut self, out: &mut Option<&mut Vec<u8>>) {
        if std::mem::take(&mut self.pending_return)
            && let Some(out) = out.as_deref_mut()
        {
            out.push(b'\n');
        }
    }
}

/// Render `retained[from..]` from a filter state at `retained[0]`.
pub(crate) fn readable_from(checkpoint: &TextFilter, retained: &[u8], from: usize) -> String {
    let from = from.min(retained.len());
    if from == retained.len() {
        return String::new();
    }
    let mut filter = checkpoint.clone();
    filter.advance(&retained[..from]);
    let mut out = Vec::with_capacity(retained.len() - from);
    filter.push(&retained[from..], &mut out);
    String::from_utf8_lossy(&out).into_owned()
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

    /// `printf 'x\033]y'` in zsh, then `echo AFTER-$((2+2))`: the OSC never
    /// ends, and the next prompt's colour is what makes tmux give up on it.
    #[test]
    fn an_unterminated_osc_ends_at_the_next_escape() {
        let text = filtered(&[
            b"printf 'x\\033]y'\r\n",
            b"x\x1b]y",
            b"\x1b[0m$ ",
            b"\x1b[32mecho\x1b[39m AFTER-$((2+2))\r\n",
            b"AFTER-4\r\n",
        ]);
        assert_eq!(text, "printf 'x\\033]y'\nx$ echo AFTER-$((2+2))\nAFTER-4\n");
    }

    #[test]
    fn an_escape_inside_an_osc_ends_it() {
        assert_eq!(filtered(&[b"a\x1b]0;title\x1b(Bb"]), "ab");
    }

    #[test]
    fn cancel_and_substitute_abandon_a_sequence() {
        assert_eq!(filtered(&[b"a\x1b]0;title\x18b\x1b[1\x1ac"]), "abc");
    }

    #[test]
    fn bel_ends_an_osc_but_not_an_application_program_command() {
        assert_eq!(filtered(&[b"a\x1b_apc\x07still apc\x1b\\b"]), "ab");
    }

    #[test]
    fn a_device_control_string_ends_only_at_string_terminator() {
        assert_eq!(filtered(&[b"a\x1bPq\x1b(data\x18\x07more\x1b\\b"]), "ab");
    }

    #[test]
    fn c1_string_terminator_is_string_data() {
        assert_eq!(filtered(&[b"a\x1b]0;x\x9cstill title\x07b"]), "ab");
    }

    #[test]
    fn an_escape_restarts_an_unfinished_control_sequence() {
        assert_eq!(filtered(&[b"a\x1b[3\x1b[0mb"]), "ab");
    }

    #[test]
    fn a_two_byte_sequence_is_removed_whole() {
        assert_eq!(filtered(&[b"a\x1b=b\x1b>c"]), "abc");
    }

    /// `tput sgr0` writes `ESC ( B` before `ESC [ m`.
    #[test]
    fn a_sequence_with_an_intermediate_is_removed_whole() {
        assert_eq!(filtered(&[b"a\x1b(Bb\x1b)0c"]), "abc");
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
