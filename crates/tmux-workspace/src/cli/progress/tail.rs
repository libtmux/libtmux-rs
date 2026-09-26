use std::collections::VecDeque;

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "emscripten",
    target_os = "fuchsia",
    target_os = "horizon",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
const LINE_BYTES: usize = 8192;

#[derive(Default)]
struct Stream {
    #[cfg(not(any(
        target_os = "cygwin",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "horizon",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    )))]
    parser: anstyle_parse::Parser,
    text: String,
    #[cfg(not(any(
        target_os = "cygwin",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "horizon",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    )))]
    carriage_return: bool,
}

pub(super) struct Tail {
    streams: [Stream; 2],
    completed: VecDeque<String>,
    order: [usize; 2],
    limit: usize,
}

impl Tail {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            streams: Default::default(),
            completed: VecDeque::new(),
            order: [0, 1],
            limit,
        }
    }

    #[cfg(not(any(
        target_os = "cygwin",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "horizon",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    )))]
    pub(super) fn append(&mut self, channel: usize, bytes: &[u8]) {
        let stream = &mut self.streams[channel];
        let mut line = Line {
            text: &mut stream.text,
            carriage_return: &mut stream.carriage_return,
            completed: &mut self.completed,
            limit: self.limit,
        };
        for &byte in bytes {
            stream.parser.advance(&mut line, byte);
        }
        self.order = [1 - channel, channel];
    }

    pub(super) fn visible(&self) -> impl Iterator<Item = &str> {
        let partials: Vec<_> = self
            .order
            .iter()
            .map(|&index| self.streams[index].text.as_str())
            .filter(|text| !text.is_empty())
            .collect();
        self.completed
            .iter()
            .map(String::as_str)
            .chain(partials.iter().copied())
            .skip((self.completed.len() + partials.len()).saturating_sub(self.limit))
            .collect::<Vec<_>>()
            .into_iter()
    }
}

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "emscripten",
    target_os = "fuchsia",
    target_os = "horizon",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
struct Line<'a> {
    text: &'a mut String,
    carriage_return: &'a mut bool,
    completed: &'a mut VecDeque<String>,
    limit: usize,
}

#[cfg(not(any(
    target_os = "cygwin",
    target_os = "emscripten",
    target_os = "fuchsia",
    target_os = "horizon",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
impl anstyle_parse::Perform for Line<'_> {
    fn print(&mut self, character: char) {
        if character.is_control() {
            return;
        }
        if *self.carriage_return {
            self.text.clear();
            *self.carriage_return = false;
        }
        if self.text.len() + character.len_utf8() <= LINE_BYTES {
            self.text.push(character);
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\r' => *self.carriage_return = true,
            b'\n' => {
                self.completed.push_back(std::mem::take(self.text));
                *self.carriage_return = false;
                while self.completed.len() > self.limit {
                    self.completed.pop_front();
                }
            }
            b'\t' => self.print(' '),
            _ => {}
        }
    }
}

#[cfg(all(
    test,
    not(any(
        target_os = "cygwin",
        target_os = "emscripten",
        target_os = "fuchsia",
        target_os = "horizon",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))
))]
mod tests {
    use super::*;

    #[test]
    fn chunks_preserve_unicode_crlf_and_stream_boundaries_without_terminal_controls() {
        let mut tail = Tail::new(4);
        for byte in "out尾\r\n\x1b[31mred\x1b[0m\x1b]0;title\x07\rreplace".as_bytes() {
            tail.append(0, &[*byte]);
        }
        tail.append(1, b"error\r");
        tail.append(1, b"\nend\ttext");
        assert_eq!(
            tail.visible().collect::<Vec<_>>(),
            ["out尾", "error", "replace", "end text"]
        );
    }

    #[test]
    fn tails_bound_long_lines_and_unterminated_osc_and_reset_at_newline() {
        let mut tail = Tail::new(2);
        tail.append(0, &vec![b'x'; LINE_BYTES * 3]);
        assert_eq!(tail.visible().next().map(str::len), Some(LINE_BYTES));
        tail.append(0, b"\nnext\nlast\n");
        assert_eq!(tail.visible().collect::<Vec<_>>(), ["next", "last"]);
        tail.append(1, b"\x1b]0;");
        tail.append(1, &vec![b'x'; LINE_BYTES * 3]);
        tail.append(1, b"\x07visible");
        assert_eq!(tail.visible().collect::<Vec<_>>(), ["last", "visible"]);
    }
}
