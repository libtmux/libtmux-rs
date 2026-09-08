//! Supported agent client catalog.

use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::ClientConfig;

/// Canonical client names in transaction order.
pub const CLIENT_NAMES: [&str; 8] = [
    "claude", "codex", "cursor", "gemini", "grok", "agy", "opencode", "pi",
];

/// Canonical supported client names in transaction order.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientName {
    /// Claude Code.
    Claude,
    /// Codex CLI.
    Codex,
    /// Cursor agent CLI.
    Cursor,
    /// Gemini CLI.
    Gemini,
    /// Grok CLI.
    Grok,
    /// Antigravity CLI, canonically named `agy`.
    Agy,
    /// opencode.
    Opencode,
    /// pi with the `pi-mcp-adapter` extension.
    Pi,
}

impl ClientName {
    /// Canonical lowercase selector.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini",
            Self::Grok => "grok",
            Self::Agy => "agy",
            Self::Opencode => "opencode",
            Self::Pi => "pi",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "cursor" => Some(Self::Cursor),
            "gemini" => Some(Self::Gemini),
            "grok" => Some(Self::Grok),
            "agy" | "antigravity" => Some(Self::Agy),
            "opencode" => Some(Self::Opencode),
            "pi" => Some(Self::Pi),
            _ => None,
        }
    }
}

/// Isolated home/config/state roots used to derive every route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Paths {
    /// Absolute home directory.
    pub home: PathBuf,
    /// Absolute XDG configuration root.
    pub config_home: PathBuf,
    /// Absolute XDG state root.
    pub state_home: PathBuf,
}

impl Paths {
    /// Construct paths from three explicit absolute roots.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownClient`] when any root is relative. The error type is
    /// shared with selector parsing so callers have one catalog failure path.
    pub fn from_roots(
        home: impl AsRef<Path>,
        config_home: impl AsRef<Path>,
        state_home: impl AsRef<Path>,
    ) -> Result<Self, UnknownClient> {
        let paths = Self {
            home: home.as_ref().to_path_buf(),
            config_home: config_home.as_ref().to_path_buf(),
            state_home: state_home.as_ref().to_path_buf(),
        };
        if [&paths.home, &paths.config_home, &paths.state_home]
            .iter()
            .any(|path| !path.is_absolute())
        {
            return Err(UnknownClient("configuration roots must be absolute".into()));
        }
        Ok(paths)
    }

    /// Derive roots from `HOME` and the absolute XDG base-directory variables.
    ///
    /// # Errors
    ///
    /// Returns [`UnknownClient`] when `HOME` is absent or relative.
    pub fn discover() -> Result<Self, UnknownClient> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| UnknownClient("HOME must name an absolute directory".into()))?;
        let config_home =
            absolute_environment("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config"));
        let state_home =
            absolute_environment("XDG_STATE_HOME").unwrap_or_else(|| home.join(".local/state"));
        Self::from_roots(home, config_home, state_home)
    }

    /// Shared lock directory used by every language port's developer swapper.
    #[must_use]
    pub fn lock_dir(&self) -> PathBuf {
        self.state_home.join("libtmux-mcp-dev/swap")
    }

    /// Shared cross-port transaction lock.
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.lock_dir().join("state.lock")
    }

    /// Rust-owned private recovery directory.
    #[must_use]
    pub fn state_dir(&self) -> PathBuf {
        self.lock_dir().join("rust")
    }

    /// Checksummed recovery ledger path.
    #[must_use]
    pub fn state_file(&self) -> PathBuf {
        self.state_dir().join("state.json")
    }
}

/// One concrete client route derived from [`Paths`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Client {
    /// Canonical client name.
    pub name: ClientName,
    /// Executable used for presence detection.
    pub binary: &'static str,
    /// Global configuration path owned by this tool.
    pub config_path: PathBuf,
    /// Format and entry dialect.
    pub config: ClientConfig,
}

/// Build all client routes in canonical transaction order.
#[must_use]
pub fn known_clients(paths: &Paths) -> Vec<Client> {
    let definitions = [
        (
            ClientName::Claude,
            "claude",
            paths.home.join(".claude.json"),
        ),
        (
            ClientName::Codex,
            "codex",
            paths.home.join(".codex/config.toml"),
        ),
        (
            ClientName::Cursor,
            "cursor-agent",
            paths.home.join(".cursor/mcp.json"),
        ),
        (
            ClientName::Gemini,
            "gemini",
            paths.home.join(".gemini/settings.json"),
        ),
        (
            ClientName::Grok,
            "grok",
            paths.home.join(".grok/config.toml"),
        ),
        (
            ClientName::Agy,
            "agy",
            paths.home.join(".gemini/config/mcp_config.json"),
        ),
        (
            ClientName::Opencode,
            "opencode",
            paths.config_home.join("opencode/opencode.jsonc"),
        ),
        (ClientName::Pi, "pi", paths.home.join(".pi/agent/mcp.json")),
    ];
    definitions
        .into_iter()
        .filter_map(|(name, binary, config_path)| {
            ClientConfig::for_name(name.as_str()).map(|config| Client {
                name,
                binary,
                config_path,
                config,
            })
        })
        .collect()
}

/// Select concrete clients with alias normalization and canonical ordering.
///
/// # Errors
///
/// Returns [`UnknownClient`] for any unsupported selector.
pub fn select_clients<'a, T: AsRef<str>>(
    clients: &'a [Client],
    selectors: &[T],
) -> Result<Vec<&'a Client>, UnknownClient> {
    if selectors.is_empty() {
        return Ok(clients.iter().collect());
    }
    let mut wanted = [false; CLIENT_NAMES.len()];
    for selector in selectors {
        let raw = selector.as_ref();
        let name = ClientName::parse(raw).ok_or_else(|| UnknownClient(raw.to_owned()))?;
        wanted[CLIENT_NAMES
            .iter()
            .position(|candidate| *candidate == name.as_str())
            .ok_or_else(|| UnknownClient(raw.to_owned()))?] = true;
    }
    Ok(clients
        .iter()
        .filter(|client| {
            CLIENT_NAMES
                .iter()
                .position(|candidate| *candidate == client.name.as_str())
                .is_some_and(|index| wanted[index])
        })
        .collect())
}

/// An invalid client selector.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnknownClient(String);

impl fmt::Display for UnknownClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unknown client {:?}", self.0)
    }
}

impl Error for UnknownClient {}

/// Normalize, deduplicate, and sort client selectors in canonical order.
///
/// An empty selector list means every supported client. `antigravity` is an
/// alias for `agy`.
///
/// # Errors
///
/// Returns [`UnknownClient`] when any selector is unsupported.
pub fn select_client_names<T: AsRef<str>>(
    selectors: &[T],
) -> Result<Vec<&'static str>, UnknownClient> {
    if selectors.is_empty() {
        return Ok(CLIENT_NAMES.to_vec());
    }

    let mut wanted = [false; CLIENT_NAMES.len()];
    for selector in selectors {
        let raw = selector.as_ref();
        let canonical = if raw == "antigravity" { "agy" } else { raw };
        let index = CLIENT_NAMES
            .iter()
            .position(|candidate| *candidate == canonical)
            .ok_or_else(|| UnknownClient(raw.to_owned()))?;
        wanted[index] = true;
    }

    Ok(CLIENT_NAMES
        .iter()
        .enumerate()
        .filter_map(|(index, name)| wanted[index].then_some(*name))
        .collect())
}

fn absolute_environment(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}
