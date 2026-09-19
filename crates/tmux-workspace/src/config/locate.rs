//! Finding where in a document a key path sits.
//!
//! `YamlLoader` keeps no positions, so a problem found in the loaded tree is
//! placed by scanning the source a second time, and only when there is one.

use std::collections::HashMap;

use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser};
use yaml_rust2::scanner::{Marker, TScalarStyle};

/// The 1-based line and column of the node at `path`, or of the nearest
/// ancestor the document has: a missing key is reported where its mapping is.
pub(super) fn locate(source: &str, path: &str) -> (usize, usize) {
    let mut recorder = Recorder {
        lines: source.lines().collect(),
        frames: Vec::new(),
        positions: HashMap::new(),
    };
    // The same source already loaded without error, so this pass cannot fail.
    let _ = Parser::new_from_str(source).load(&mut recorder, true);

    let mut path = path;
    loop {
        if let Some(&position) = recorder.positions.get(path) {
            return position;
        }
        if path.is_empty() {
            return (1, 1);
        }
        path = &path[..path.rfind(['.', '[']).unwrap_or(0)];
    }
}

/// `Marker::col` counts from zero; its own `Display` adds one too.
fn position(mark: Marker) -> (usize, usize) {
    (mark.line(), mark.col() + 1)
}

/// Records the position of every value node under the key path the parsing
/// code names it by: `windows[0].panes[1].shell_command`.
#[derive(Debug)]
struct Recorder<'source> {
    lines: Vec<&'source str>,
    frames: Vec<Frame>,
    positions: HashMap<String, (usize, usize)>,
}

#[derive(Debug)]
enum Frame {
    Mapping {
        path: String,
        key: Option<String>,
        key_mark: Option<Marker>,
        awaiting_key: bool,
    },
    Sequence {
        path: String,
        mark: Marker,
        next: usize,
        /// The line the previous entry began on.
        previous: Option<usize>,
    },
}

/// Stands in for the path of a container used as a mapping key, so nothing
/// inside one can shadow a real key.
const KEY_NODE: &str = "\0key";

impl Recorder<'_> {
    /// The path of a node starting now, or `None` when it is a mapping key.
    fn child_path(&self) -> Option<String> {
        match self.frames.last() {
            None => Some(String::new()),
            Some(Frame::Mapping {
                awaiting_key: true, ..
            }) => None,
            Some(Frame::Mapping { path, key, .. }) => {
                let key = key.as_deref().unwrap_or(KEY_NODE);
                Some(if path.is_empty() {
                    key.to_owned()
                } else {
                    format!("{path}.{key}")
                })
            }
            Some(Frame::Sequence { path, next, .. }) => Some(format!("{path}[{next}]")),
        }
    }

    /// Where a node the parser implied (`-` or `key:` with nothing after) is.
    ///
    /// The parser marks such a node at a token it peeked past, often on a
    /// later line, so it is placed at its key or at its entry's `-` instead.
    fn implied(&self, mark: Marker) -> (usize, usize) {
        match self.frames.last() {
            Some(Frame::Mapping {
                key_mark: Some(key),
                ..
            }) => position(*key),
            Some(Frame::Sequence {
                mark: first,
                previous: None,
                ..
            }) => position(*first),
            Some(Frame::Sequence {
                previous: Some(previous),
                ..
            }) => self
                .lines
                .iter()
                .enumerate()
                .skip(*previous)
                .find_map(|(index, line)| {
                    let indent = line.len() - line.trim_start().len();
                    line.trim_start()
                        .starts_with('-')
                        .then_some((index + 1, indent + 1))
                })
                .unwrap_or_else(|| position(mark)),
            _ => position(mark),
        }
    }

    fn record(&mut self, mark: Marker, implied: bool) -> Option<String> {
        let path = self.child_path();
        let at = if implied && path.is_some() {
            self.implied(mark)
        } else {
            position(mark)
        };
        if let Some(path) = &path {
            self.positions.entry(path.clone()).or_insert(at);
        }
        match self.frames.last_mut() {
            Some(Frame::Mapping {
                path: mapping,
                key_mark,
                awaiting_key: true,
                ..
            }) => {
                // A block mapping is marked at its first key's `:`, so it
                // takes its first key's position instead.
                if key_mark.is_none() {
                    if let Some(at) = self.positions.get_mut(mapping.as_str()) {
                        *at = position(mark);
                    }
                }
                *key_mark = Some(mark);
            }
            Some(Frame::Sequence { previous, .. }) => *previous = Some(at.0),
            _ => {}
        }
        path
    }

    /// Step the enclosing container past a node that has ended.
    fn finish(&mut self, scalar: Option<String>) {
        match self.frames.last_mut() {
            Some(Frame::Mapping {
                key, awaiting_key, ..
            }) => {
                if *awaiting_key {
                    *key = scalar;
                }
                *awaiting_key = !*awaiting_key;
            }
            Some(Frame::Sequence { next, .. }) => *next += 1,
            None => {}
        }
    }
}

impl MarkedEventReceiver for Recorder<'_> {
    fn on_event(&mut self, event: Event, mark: Marker) {
        match event {
            Event::Scalar(text, style, ..) => {
                // A plain scalar cannot be empty in the source, so an empty
                // one is a node the parser implied.
                self.record(mark, text.is_empty() && style == TScalarStyle::Plain);
                self.finish(Some(text));
            }
            Event::Alias(_) => {
                self.record(mark, false);
                self.finish(None);
            }
            Event::MappingStart(..) => {
                let path = self
                    .record(mark, false)
                    .unwrap_or_else(|| KEY_NODE.to_owned());
                self.frames.push(Frame::Mapping {
                    path,
                    key: None,
                    key_mark: None,
                    awaiting_key: true,
                });
            }
            Event::SequenceStart(..) => {
                let path = self
                    .record(mark, false)
                    .unwrap_or_else(|| KEY_NODE.to_owned());
                self.frames.push(Frame::Sequence {
                    path,
                    mark,
                    next: 0,
                    previous: None,
                });
            }
            Event::MappingEnd | Event::SequenceEnd => {
                self.frames.pop();
                self.finish(None);
            }
            _ => {}
        }
    }
}
