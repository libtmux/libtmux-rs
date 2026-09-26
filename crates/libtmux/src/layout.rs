use std::ffi::OsStr;

use crate::{Command, Error, Layout, Server, TmuxVersion};

const NAMES: [Layout; 7] = [
    Layout::EvenHorizontal,
    Layout::EvenVertical,
    Layout::MainHorizontal,
    Layout::MainVertical,
    Layout::Tiled,
    Layout::MainHorizontalMirrored,
    Layout::MainVerticalMirrored,
];

fn named(input: &[u8], mirrored: bool) -> Option<Layout> {
    let names = &NAMES[..if mirrored { 7 } else { 5 }];
    if let Some(layout) = names
        .iter()
        .find(|layout| layout.as_str().as_bytes() == input)
    {
        return Some(*layout);
    }
    let mut matches = names
        .iter()
        .filter(|layout| layout.as_str().as_bytes().starts_with(input));
    let first = matches.next().copied()?;
    matches.next().is_none().then_some(first)
}

pub(crate) fn accepts(input: &[u8], panes: usize, mirrored: bool) -> bool {
    if input.is_empty() || panes == 0 {
        return false;
    }
    if named(input, mirrored).is_some() {
        return true;
    }
    if input.len() <= 5 || input[4] != b',' || !input[..4].iter().all(u8::is_ascii_hexdigit) {
        return false;
    }
    let expected = std::str::from_utf8(&input[..4])
        .ok()
        .and_then(|header| u16::from_str_radix(header, 16).ok());
    let body = &input[5..];
    let checksum = body.iter().fold(0_u16, |sum, byte| {
        sum.rotate_right(1).wrapping_add(u16::from(*byte))
    });
    if expected != Some(checksum) {
        return false;
    }
    let mut parser = Parser {
        input: body,
        offset: 0,
        leaves: 0,
    };
    parser.cell(0) && parser.offset == body.len() && parser.leaves >= panes
}

pub(crate) fn prepare<'a>(
    layouts: impl IntoIterator<Item = (&'a OsStr, usize)>,
) -> Result<Vec<(&'a OsStr, usize)>, Error> {
    let mut pending = Vec::new();
    for (layout, panes) in layouts {
        let bytes = layout.as_encoded_bytes();
        if bytes.starts_with(b"{") {
            pending.push((layout, panes));
            continue;
        }
        let before = accepts(bytes, panes, false);
        let after = accepts(bytes, panes, true);
        if !before && !after {
            return Err(Error::InvalidLayout {
                reason: "use a unique layout name or a checksummed nonempty tree with enough pane cells",
            });
        }
        if before != after {
            pending.push((layout, panes));
        }
    }
    Ok(pending)
}

pub(crate) fn resolve(pending: &[(&OsStr, usize)], version: &TmuxVersion) -> Result<(), Error> {
    let mirrored = version
        .behavior_release()
        .is_none_or(|release| release >= crate::since::MIRRORED_LAYOUTS);
    for (layout, panes) in pending {
        let bytes = layout.as_encoded_bytes();
        if bytes.starts_with(b"{") {
            version.require("a JSON layout string", crate::since::JSON_LAYOUTS)?;
            continue;
        }
        if !accepts(bytes, *panes, mirrored) {
            if let Some(layout) = named(bytes, true) {
                version.require(layout.as_str(), layout.minimum_release())?;
            }
            return Err(Error::InvalidLayout {
                reason: "use a layout name that is unique on the running tmux version",
            });
        }
    }
    Ok(())
}

pub(crate) async fn version(server: &Server) -> Result<TmuxVersion, Error> {
    let result = server.cmd(version_command()).await?;
    if result.success() {
        return TmuxVersion::parse_output(result.stdout());
    }
    if is_cold(result.stderr()) {
        return Ok(server.capabilities().await?.tmux_version().clone());
    }
    Err(Error::from_refused_result("display-message", &result, None))
}

fn is_cold(stderr: &[u8]) -> bool {
    let reason = stderr.trim_ascii();
    reason.starts_with(b"no server running on ")
        || (reason.starts_with(b"error connecting to ")
            && reason.ends_with(b" (No such file or directory)"))
}

pub(crate) fn version_command() -> Command {
    Command::new("display-message")
        .arg("-p")
        .arg("--")
        .arg("tmux #{version}")
}

struct Parser<'a> {
    input: &'a [u8],
    offset: usize,
    leaves: usize,
}

impl Parser<'_> {
    fn take(&mut self, byte: u8) -> bool {
        if self.input.get(self.offset) != Some(&byte) {
            return false;
        }
        self.offset += 1;
        true
    }

    fn number(&mut self) -> bool {
        let start = self.offset;
        let mut value = 0_u32;
        while let Some(digit) = self.input.get(self.offset) {
            if !digit.is_ascii_digit() {
                break;
            }
            let Some(next) = value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u32::from(digit - b'0')))
            else {
                return false;
            };
            value = next;
            self.offset += 1;
        }
        self.offset > start
    }

    fn cell(&mut self, depth: usize) -> bool {
        if depth > 256
            || !self.number()
            || !self.take(b'x')
            || !self.number()
            || !self.take(b',')
            || !self.number()
            || !self.take(b',')
            || !self.number()
        {
            return false;
        }
        let saved = self.offset;
        if self.take(b',') && (!self.number() || self.input.get(self.offset) == Some(&b'x')) {
            self.offset = saved;
        }
        let close = if self.take(b'{') {
            b'}'
        } else if self.take(b'[') {
            b']'
        } else {
            self.leaves += 1;
            return true;
        };
        if !self.cell(depth + 1) {
            return false;
        }
        while self.take(b',') {
            if !self.cell(depth + 1) {
                return false;
            }
        }
        self.take(close)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serialized(body: &[u8]) -> Vec<u8> {
        let checksum = body.iter().fold(0_u16, |sum, byte| {
            sum.rotate_right(1).wrapping_add(u16::from(*byte))
        });
        [format!("{checksum:04x},").as_bytes(), body].concat()
    }

    #[test]
    fn layout_preflight_shared_corpus_keeps_geometry_with_tmux() {
        let corpus: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/layout-preflight.json")).unwrap();
        for case in corpus.as_array().unwrap() {
            let id = case["id"].as_str().unwrap();
            let layout = case["layout"].as_str().unwrap();
            let panes = usize::try_from(case["pane_count"].as_u64().unwrap()).unwrap();
            for (version, mirrored) in [("3.2a", false), ("3.3a", false), ("3.7c", true)] {
                let geometry = matches!(
                    id,
                    "bad-inner-size" | "nested-invalid-width" | "nested-short-parent"
                );
                let expected = !layout.is_empty()
                    && (geometry || case["expected_valid"][version].as_bool().unwrap());
                assert_eq!(
                    accepts(layout.as_bytes(), panes, mirrored),
                    expected,
                    "{version} {id}"
                );
            }
        }
    }

    #[test]
    fn layout_preflight_unsigned_fields_depth_and_bytes() {
        assert!(accepts(
            &serialized(b"4294967295x0,0,0,4294967295"),
            1,
            false
        ));
        for body in [
            b"4294967296x1,0,0".as_slice(),
            b"1x1,0,4294967296",
            b"1x1,0,0,4294967296",
            b"1x1,0,0{}",
            b"1x1,0,0[]",
            b"\xffx1,0,0",
        ] {
            assert!(!accepts(&serialized(body), 1, true));
        }
        let nested = format!("{}1x1,0,0{}", "1x1,0,0{".repeat(256), "}".repeat(256));
        assert!(accepts(&serialized(nested.as_bytes()), 1, true));
        assert!(!accepts(
            &serialized(format!("1x1,0,0{{{nested}}}").as_bytes()),
            1,
            true
        ));
        let long = format!("{}1x1,0,0", "0".repeat(9000));
        assert!(accepts(&serialized(long.as_bytes()), 1, true));
        assert!(!accepts(b"tiled", 0, true));
    }

    #[test]
    fn layout_preflight_cold_diagnostics_exclude_live_or_permission_failures() {
        for reason in [
            b"no server running on /socket\n".as_slice(),
            b"error connecting to /socket (No such file or directory)\n",
        ] {
            assert!(is_cold(reason));
        }
        for reason in [
            b"server exited unexpectedly".as_slice(),
            b"error connecting to /socket (Permission denied)\n",
            b"protocol version mismatch",
            b"can't find session: absent",
        ] {
            assert!(!is_cold(reason));
        }
    }

    #[test]
    fn layout_preflight_retains_native_version_and_error_semantics() {
        let old = TmuxVersion::parse_output(b"tmux 3.2a\n").unwrap();
        let new = TmuxVersion::parse_output(b"tmux 3.7c\n").unwrap();
        let json = [(OsStr::new(r#"{"V":2,"L":[]}"#), 1)];
        let pending = prepare(json).unwrap();
        assert_eq!(
            resolve(&pending, &new).unwrap_err().kind(),
            crate::ErrorKind::UnsupportedVersion
        );
        for output in ["tmux 3.8-rc\n", "tmux 3.8\n", "tmux next-3.9\n"] {
            let json_capable = TmuxVersion::parse_output(output.as_bytes()).unwrap();
            resolve(&pending, &json_capable).unwrap();
        }
        let mirrored = [(OsStr::new("main-horizontal-mirrored"), 1)];
        assert_eq!(
            resolve(&prepare(mirrored).unwrap(), &old)
                .unwrap_err()
                .kind(),
            crate::ErrorKind::UnsupportedVersion
        );
        resolve(&prepare(mirrored).unwrap(), &new).unwrap();
        let abbreviation = [(OsStr::new("main-h"), 1)];
        resolve(&prepare(abbreviation).unwrap(), &old).unwrap();
        let error = resolve(&prepare(abbreviation).unwrap(), &new).unwrap_err();
        assert_eq!(error.kind(), crate::ErrorKind::InvalidInput);
        assert!(!error.is_transient());
    }
}
