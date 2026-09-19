mod tail;
mod template;

use std::io::{self, IsTerminal, Write};

use clap::{ArgMatches, parser::ValueSource};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{CliError, Result, discovery, normalize, output};
use tail::Tail;
use template::{State, Template};

const MAX_ROWS: usize = 128;

#[derive(PartialEq, Eq)]
enum Drawing {
    Ready,
    PartialLine,
    Stopped,
}

pub(super) struct Progress {
    size: (u16, u16),
    lines: usize,
    #[cfg(unix_process_observer)]
    same_stdout: bool,
    color: bool,
    drawing: Drawing,
    template: Template,
    state: State,
    tail: Tail,
    frame: Vec<String>,
}

fn geometry() -> Option<(u16, u16)> {
    let size = rustix::termios::tcgetwinsize(io::stderr()).ok()?;
    (size.ws_col >= 2 && size.ws_row >= 2).then_some((size.ws_col, size.ws_row))
}

#[cfg(unix_process_observer)]
fn shared_terminal() -> bool {
    if !io::stdout().is_terminal() {
        return false;
    }
    match (
        rustix::fs::fstat(io::stdout()),
        rustix::fs::fstat(io::stderr()),
    ) {
        (Ok(out), Ok(err)) => {
            (out.st_dev, out.st_ino, out.st_rdev) == (err.st_dev, err.st_ino, err.st_rdev)
        }
        _ => false,
    }
}

impl Progress {
    pub(super) fn new(args: &ArgMatches, machine: bool) -> Result<Option<Self>> {
        if machine
            || !io::stderr().is_terminal()
            || args.get_flag("no-progress")
            || std::env::var("TMUXP_PROGRESS").as_deref() == Ok("0")
            || std::env::var("TERM").as_deref() == Ok("dumb")
        {
            return Ok(None);
        }
        let Some(size) = geometry() else {
            return Ok(None);
        };
        let lines = if args.value_source("progress-lines") == Some(ValueSource::CommandLine) {
            args.get_one::<i32>("progress-lines").copied().unwrap_or(3)
        } else if let Some(value) = std::env::var_os("TMUXP_PROGRESS_LINES") {
            value
                .to_str()
                .and_then(|value| value.parse::<i32>().ok())
                .filter(|value| *value >= -1)
                .ok_or_else(|| {
                    CliError::usage("TMUXP_PROGRESS_LINES must be an integer of -1 or greater")
                })?
        } else {
            3
        };
        let format = args
            .get_one::<String>("progress-format")
            .cloned()
            .or_else(|| std::env::var("TMUXP_PROGRESS_FORMAT").ok())
            .unwrap_or_else(|| "default".into());
        let available = usize::from(size.1 - 2).min(MAX_ROWS);
        let lines = usize::try_from(lines).map_or(available, |lines| lines.min(available));
        Ok(Some(Self {
            size,
            lines,
            #[cfg(unix_process_observer)]
            same_stdout: shared_terminal(),
            color: output::color_enabled(args, true),
            drawing: Drawing::Ready,
            template: Template::new(&format),
            state: State::default(),
            tail: Tail::new(lines),
            frame: Vec::new(),
        }))
    }

    pub(super) fn start(&mut self, workspace: &normalize::Workspace) -> Result<()> {
        self.clear()?;
        self.tail = Tail::new(self.lines);
        self.state = State {
            path: discovery::masked(&workspace.source),
            session: workspace.name.clone(),
            windows: if workspace.bridge {
                0
            } else {
                workspace.windows.len()
            },
            total_panes: if workspace.bridge {
                0
            } else {
                workspace
                    .windows
                    .iter()
                    .map(|window| window.panes.len())
                    .sum()
            },
            status: if workspace.bridge {
                "Python extensions"
            } else {
                ""
            },
            ..State::default()
        };
        self.draw()
    }

    pub(super) fn window(&mut self, ordinal: usize, window: &normalize::Window) -> Result<()> {
        self.state
            .window
            .clone_from(&window.name.clone().unwrap_or_default());
        self.state.window_index = ordinal;
        self.state.panes = window.panes.len();
        self.state.pane_index = 0;
        self.state.panes_done = 0;
        self.draw()
    }

    pub(super) fn pane(&mut self, ordinal: usize) -> Result<()> {
        self.state.pane_index = ordinal;
        self.draw()
    }

    pub(super) fn pane_done(&mut self) -> Result<()> {
        self.state.panes_done += 1;
        self.state.total_done += 1;
        self.draw()
    }

    pub(super) fn window_done(&mut self) -> Result<()> {
        self.state.windows_done += 1;
        self.draw()
    }

    pub(super) fn finish(&mut self, reused: bool, delegated: bool) -> Result<()> {
        self.state.status = if reused {
            "Reused"
        } else if delegated {
            "Python extensions completed"
        } else {
            "Completed"
        };
        if reused {
            self.state.windows = 0;
            self.state.total_panes = 0;
        }
        self.draw()
    }

    fn unchanged(&mut self) -> bool {
        if self.drawing != Drawing::Stopped && geometry() != Some(self.size) {
            self.drawing = Drawing::Stopped;
            self.frame.clear();
        }
        self.drawing != Drawing::Stopped
    }

    pub(super) fn clear(&mut self) -> Result<()> {
        if self.unchanged() && !self.frame.is_empty() {
            let mut error = io::stderr().lock();
            for _ in &self.frame {
                write!(error, "\x1b[1A\r\x1b[2K")?;
            }
            error.flush()?;
            self.frame.clear();
        }
        Ok(())
    }

    #[cfg(unix_process_observer)]
    pub(super) fn output(&mut self, stream: &str, text: &str) -> Result<bool> {
        if !self.unchanged() || (stream == "stdout" && !self.same_stdout) {
            return Ok(false);
        }
        if self.lines > 0 {
            self.tail
                .append(usize::from(stream == "stderr"), text.as_bytes());
        } else {
            self.clear()?;
            if stream == "stderr" {
                io::stderr().write_all(text.as_bytes())?;
                io::stderr().flush()?;
            } else {
                io::stdout().write_all(text.as_bytes())?;
                io::stdout().flush()?;
            }
            self.drawing = if text.ends_with('\n') {
                Drawing::Ready
            } else {
                Drawing::PartialLine
            };
        }
        self.draw()?;
        Ok(true)
    }

    fn draw(&mut self) -> Result<()> {
        if !self.unchanged() || self.drawing == Drawing::PartialLine {
            return Ok(());
        }
        let mut heading = self.template.render(&self.state);
        if !self.state.status.is_empty() {
            heading.push_str(" · ");
            heading.push_str(self.state.status);
        }
        let frame: Vec<_> = std::iter::once(heading.as_str())
            .chain(self.tail.visible())
            .map(|line| clip(line, usize::from(self.size.0 - 1)))
            .collect();
        if frame == self.frame {
            return Ok(());
        }
        self.clear()?;
        let mut error = io::stderr().lock();
        for (index, line) in frame.iter().enumerate() {
            if self.color {
                let style = if index == 0 { "1;36" } else { "2" };
                write!(error, "\x1b[{style}m{line}\x1b[0m\r\n")?;
            } else {
                write!(error, "{line}\r\n")?;
            }
        }
        error.flush()?;
        self.frame = frame;
        Ok(())
    }
}

fn clip(text: &str, width: usize) -> String {
    let safe: String = text
        .chars()
        .filter(|character| !character.is_control())
        .take(8192)
        .collect();
    let mut cells = 0;
    safe.graphemes(true)
        .take_while(|cluster| {
            cells += cluster.width();
            cells <= width
        })
        .collect()
}
