#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["tomlkit>=0.13"]
# ///
"""Swap MCP server configs across every installed agent CLI.

Use when you want every installed agent CLI to run a particular build of
``tmux-mcp`` -- the one you are editing, a compiled profile, or a
published release -- instead of whatever they point at now. ``use``
rewrites each CLI's config; ``revert`` restores from the timestamped
backup the swap wrote. Swapping a layer that is already swapped keeps
that first backup rather than taking a new one, so ``revert`` always
lands on the pre-swap config.

Sources
-------
``--source`` picks where the server comes from:

- ``debug`` / ``release`` build the crate and name the binary in
  ``target/<profile>/``, so an agent spawns it directly with no build
  step in front of the handshake. This is the default (``debug``).
- ``run`` launches through ``cargo run``, which rebuilds on every start.
  Current source with nothing to remember, at the cost of a build check
  per launch -- and a slow first launch after a change can outlast a
  client's handshake timeout.
- ``published`` installs a crates.io release under its own root, so
  swapping between releases does not reinstall over the previous one.
- ``path`` takes a binary you name with ``--bin``, wherever it came
  from.

Defaults are derived from the crate's ``Cargo.toml``:

- server name = package name with a trailing ``-mcp`` stripped
  (``tmux-mcp`` -> ``tmux``)
- binary name = the crate's ``[[bin]]`` entry, or its ``src/bin`` stem

Examples
--------
```console
$ uv run scripts/mcp_swap.py detect
$ uv run scripts/mcp_swap.py status
$ uv run scripts/mcp_swap.py use --dry-run
$ uv run scripts/mcp_swap.py use --source release
$ uv run scripts/mcp_swap.py use --source run
$ uv run scripts/mcp_swap.py use --source published --version 0.1.0-alpha.3
$ uv run scripts/mcp_swap.py revert
```

Scope
-----
This script is best-effort and intentionally narrow:

- **Global configs only.** Writes to ``~/.cursor/mcp.json``,
  ``~/.claude.json``, ``~/.codex/config.toml``,
  ``~/.gemini/settings.json``, ``~/.grok/config.toml`` (TOML
  ``mcp_servers``, same shape as Codex),
  ``~/.gemini/config/mcp_config.json`` (agy / Antigravity CLI, JSON
  ``mcpServers`` — the shared-config file the CLI reads, sibling to the
  ``config.json`` it loads at startup),
  ``$XDG_CONFIG_HOME/opencode/opencode.jsonc`` (JSONC ``mcp``, comments
  preserved) and ``~/.pi/agent/mcp.json`` (JSONC too -- the adapter that
  reads it strips comments). Workspace / project-local
  configs (``$PWD/.cursor/mcp.json``, ``$PWD/.gemini/settings.json``,
  ``$PWD/opencode.json``, per-project ``projects.<abs>.mcpServers``
  entries inside ``~/.claude.json`` *are* recognised for Claude only)
  are NOT walked — workspace files for the others are silently ignored.
  When workspace precedence matters, run the CLI's own
  ``cursor mcp add ...`` / ``gemini mcp add ...`` directly. opencode has
  no non-interactive project-scope add -- ``opencode mcp add`` writes the
  global file -- so edit ``$PWD/opencode.json`` by hand for that.

- **opencode reads three global files.** ``config.json``,
  ``opencode.json`` and ``opencode.jsonc`` in the same directory are all
  loaded and merged, with ``.jsonc`` winning. This script owns
  ``.jsonc`` — the file opencode itself writes to — so its entry is the
  one that takes effect. A stale ``mcp.<name>`` left in a sibling
  ``opencode.json`` still merges underneath rather than being shadowed
  outright; remove it by hand if that matters.

- **pi has no MCP client of its own.** Its README says so, and the
  released build ships no MCP code. ``~/.pi/agent/mcp.json`` is read by
  the third-party ``pi-mcp-adapter`` extension, so a swap written there
  takes effect only once that package is installed. ``detect`` says as
  much rather than reporting a swap that cannot do anything.

- **Claude scope.** ``use-local`` and ``revert`` accept
  ``--scope {user,project}``. The default ``project`` writes the
  per-project entry under ``projects[<abs-repo>].mcpServers`` —
  only the current repo's directory sees the swap, matching
  pre-flag behaviour. ``--scope user`` writes Claude's top-level
  ``mcpServers`` fallback so every project that has no per-project
  override picks up the swap; useful when QA-ing a branch across
  many directories. Every other CLI here has no per-project layer in
  the config file this script writes; the flag is silently coerced to
  ``user`` for them. Both Claude scopes can coexist with
  independent backups; full ``revert`` unwinds in LIFO order.
- **Simple binary detection.** Probing is ``shutil.which(<binary>)``
  plus ``<config_path>.exists()``. Custom install locations
  (Homebrew, npm prefixes, ``~/.npm-global/bin``,
  ``~/.claude/local/claude``, ``~/.gemini/local/gemini``) are picked
  up only if the binary is on ``PATH``. FastMCP's installer probes
  these locations directly; this script does not.
- **Single config shape per CLI.** No fallback paths, no merge of
  multiple sources. If your setup deviates from the defaults above,
  use the CLI's native ``mcp`` subcommand instead.
"""

from __future__ import annotations

import argparse
import contextlib
import dataclasses
import difflib
import fcntl
import hashlib
import itertools
import json
import os
import pathlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import typing as t

import tomlkit
import tomlkit.items

CLIName = t.Literal[
    "claude", "codex", "cursor", "gemini", "grok", "agy", "opencode", "pi"
]
ALL_CLIS: tuple[CLIName, ...] = (
    "claude",
    "codex",
    "cursor",
    "gemini",
    "grok",
    "agy",
    "opencode",
    "pi",
)

#: Width of the CLI-name column in ``detect`` output, derived rather
#: than hardcoded so adding a longer name cannot silently misalign it.
_CLI_COLUMN = max(len(name) for name in ALL_CLIS) + 1

#: Claude config scope: ``"user"`` targets the user/system-level top-level
#: ``mcpServers`` fallback that applies to every project without its own
#: override; ``"project"`` targets the project-level per-project
#: ``projects.<abs>.mcpServers`` node. Non-Claude CLIs have no
#: per-project scope in their config files, so for those CLIs the scope
#: is always normalised to ``"user"`` regardless of what was passed.
Scope = t.Literal["user", "project"]
ALL_SCOPES: tuple[Scope, ...] = ("user", "project")


def _normalize_scope(cli: CLIName, scope: Scope | None) -> Scope:
    """Coerce ``scope`` to the value that actually applies to ``cli``.

    Non-Claude CLIs have no per-project config layer — every write to
    them is necessarily user-level — so the flag is silently coerced to
    ``"user"`` for those. For Claude, ``None`` defaults to ``"project"``
    to preserve pre-flag behaviour where the script always wrote the
    per-project entry.
    """
    if cli != "claude":
        return "user"
    return scope if scope is not None else "project"


def _state_key(cli: CLIName, scope: Scope) -> str:
    """Compose the ``cli:scope`` key used inside the state file."""
    return f"{cli}:{scope}"


def _parse_state_key(key: str) -> tuple[CLIName, Scope] | None:
    """Decode a ``cli:scope`` state key, returning ``None`` for malformed input.

    The script declares no compatibility contract for its state file —
    schema is internal — so this only accepts the canonical
    ``f"{cli}:{scope}"`` form. Hand-edited or unrecognised keys return
    ``None`` so ``load_state`` can drop them without crashing.
    """
    if ":" not in key:
        return None
    cli_str, _, scope_str = key.partition(":")
    if cli_str in ALL_CLIS and scope_str in ALL_SCOPES:
        return cli_str, scope_str
    return None


def _parse_state_entry(
    value: dict[str, t.Any], *, allow_legacy: bool = True
) -> SwapEntry | None:
    """Build a validated state entry, optionally for read-only legacy use."""
    try:
        return _state_entry_from_document(value, allow_legacy=allow_legacy)
    except (KeyError, TypeError, ValueError):
        return None


def _xdg_state_home() -> pathlib.Path:
    """Resolve ``$XDG_STATE_HOME`` per the XDG Base Directory spec.

    Defaults to ``~/.local/state`` when the env var is unset or empty.
    State is the right XDG bucket here (vs. cache / config / data): the
    file is machine-written, must persist across runs so ``revert`` can
    locate the right backup, but is not safely deletable like cache nor
    user-edited like config.
    """
    env = os.environ.get("XDG_STATE_HOME")
    if env:
        return pathlib.Path(env)
    return pathlib.Path.home() / ".local" / "state"


# ``-dev`` suffix in the namespace makes it loud that this is dev-only
# tooling state, distinct from the ``tmux-mcp`` crate it swaps.
STATE_DIR = _xdg_state_home() / "tmux-mcp-dev" / "swap"
STATE_FILE = STATE_DIR / "state.json"

BACKUP_SUFFIX_PREFIX = ".bak.mcp-swap-"
STATE_VERSION = 1
STATE_MAX_BYTES = 256 * 1024


# ---------------------------------------------------------------------------
# Models
# ---------------------------------------------------------------------------


#: Per-entry shape a CLI expects under its server map. ``standard`` is
#: the Claude-Desktop lineage every CLI here started from — scalar
#: ``command``, sibling ``args`` list, optional ``env`` table.
#: ``claude`` is that shape plus an explicit ``type``/``env`` that
#: Claude writes even when empty. ``opencode`` packs argv into a single
#: ``command`` array and spells the environment table ``environment``.
#: Dialects exist because the shape is not implied by the file format:
#: two CLIs sharing ``fmt="json"`` can still disagree about how one
#: entry is spelled.
Dialect = t.Literal["standard", "claude", "opencode"]


@dataclasses.dataclass(frozen=True)
class CLIInfo:
    """Static descriptor for a CLI's config file and discovery heuristics."""

    name: CLIName
    binary: str
    config_path: pathlib.Path
    fmt: t.Literal["json", "jsonc", "toml"]
    #: Key path from the document root down to the mapping of server
    #: name -> entry. A path rather than a single key so a CLI that
    #: nests deeper needs no new branch in the four functions that
    #: read, write, delete and enumerate entries.
    container: tuple[str, ...]
    #: Entry shape written and read back for this CLI.
    dialect: Dialect


def _xdg_config_home() -> pathlib.Path:
    """``$XDG_CONFIG_HOME`` when absolute, else ``~/.config``.

    The spec requires these variables to be absolute and says to ignore
    them otherwise. A relative value would resolve against the working
    directory, so the swap would record a backup path that revert could
    no longer find from anywhere else.
    """
    raw = os.environ.get("XDG_CONFIG_HOME")
    if raw and pathlib.Path(raw).is_absolute():
        return pathlib.Path(raw)
    return pathlib.Path.home() / ".config"


CLIS: dict[CLIName, CLIInfo] = {
    "claude": CLIInfo(
        name="claude",
        binary="claude",
        config_path=pathlib.Path.home() / ".claude.json",
        fmt="json",
        container=("mcpServers",),
        dialect="claude",
    ),
    "codex": CLIInfo(
        name="codex",
        binary="codex",
        config_path=pathlib.Path.home() / ".codex" / "config.toml",
        fmt="toml",
        container=("mcp_servers",),
        dialect="standard",
    ),
    "cursor": CLIInfo(
        name="cursor",
        binary="cursor-agent",
        config_path=pathlib.Path.home() / ".cursor" / "mcp.json",
        fmt="json",
        container=("mcpServers",),
        dialect="standard",
    ),
    "gemini": CLIInfo(
        name="gemini",
        binary="gemini",
        config_path=pathlib.Path.home() / ".gemini" / "settings.json",
        fmt="json",
        container=("mcpServers",),
        dialect="standard",
    ),
    "grok": CLIInfo(
        name="grok",
        binary="grok",
        config_path=pathlib.Path.home() / ".grok" / "config.toml",
        fmt="toml",
        container=("mcp_servers",),
        dialect="standard",
    ),
    "agy": CLIInfo(
        name="agy",
        binary="agy",
        config_path=(pathlib.Path.home() / ".gemini" / "config" / "mcp_config.json"),
        fmt="json",
        container=("mcpServers",),
        dialect="standard",
    ),
    "opencode": CLIInfo(
        name="opencode",
        binary="opencode",
        # opencode reads config.json, opencode.json and opencode.jsonc from
        # this directory and merges all three, with .jsonc winning. It writes
        # to the first that exists, defaulting to .jsonc — so that is the one
        # file a swap can own without being shadowed.
        config_path=_xdg_config_home() / "opencode" / "opencode.jsonc",
        fmt="jsonc",
        container=("mcp",),
        dialect="opencode",
    ),
    "pi": CLIInfo(
        name="pi",
        binary="pi",
        # Read by the pi-mcp-adapter extension, not by pi itself; see
        # PI_ADAPTER_DIR. Claude-Desktop schema, so the standard dialect.
        # The adapter parses through strip-json-comments with trailing
        # commas allowed, so the file is JSONC despite the .json suffix.
        config_path=pathlib.Path.home() / ".pi" / "agent" / "mcp.json",
        fmt="jsonc",
        container=("mcpServers",),
        dialect="standard",
    ),
}

#: Written into an opencode config this script creates from nothing.
#: opencode injects the same line itself on first load; seeding it here
#: keeps the swap from being followed by a surprise rewrite.
OPENCODE_SCHEMA_URL = "https://opencode.ai/config.json"

#: pi ships no MCP client — its README says "No MCP" outright, and the
#: released build contains no MCP code at all. MCP reaches pi only
#: through the third-party ``pi-mcp-adapter`` extension, which is what
#: reads ``~/.pi/agent/mcp.json``. The swap writes that file because it
#: is the one pi-family location with a settled schema, but until the
#: adapter is installed pi does not read it, so ``detect`` says so
#: instead of reporting a swap that cannot take effect.
PI_ADAPTER_DIR = (
    pathlib.Path.home() / ".pi" / "agent" / "npm" / "node_modules" / "pi-mcp-adapter"
)
PI_ADAPTER_HINT = "needs the pi-mcp-adapter package; pi has no built-in MCP client"


#: A ``--from`` argument pointing at a pull request's head commit.
#: GitHub publishes ``refs/pull/<n>/head`` on the *base* repository, so
#: one URL serves same-repo and fork pull requests alike.


@dataclasses.dataclass
class McpServerSpec:
    """The portable shape shared across CLI configs."""

    command: str
    args: list[str] = dataclasses.field(default_factory=list)
    env: dict[str, str] = dataclasses.field(default_factory=dict)

    def to_entry_dict(self, dialect: Dialect = "standard") -> dict[str, t.Any]:
        """Serialize to the entry shape ``dialect`` expects."""
        # Claude's format always includes ``type`` and ``env`` (even when
        # empty); the standard shape omits both when there is nothing to say.
        if dialect == "claude":
            return {
                "type": "stdio",
                "command": self.command,
                "args": list(self.args),
                "env": dict(self.env),
            }
        if dialect == "opencode":
            # One array for argv, and the table is "environment" -- an
            # "env" key here is dropped in silence, and a scalar command
            # is a decode error that takes the whole config down with it.
            local: dict[str, t.Any] = {
                "type": "local",
                "command": [self.command, *self.args],
            }
            if self.env:
                local["environment"] = dict(self.env)
            return local
        out: dict[str, t.Any] = {"command": self.command, "args": list(self.args)}
        if self.env:
            out["env"] = dict(self.env)
        return out

    def cargo_manifest_path(self) -> pathlib.Path | None:
        """Extract ``--manifest-path`` from a ``cargo run`` spec, if any."""
        if self.command != "cargo":
            return None
        try:
            i = self.args.index("--manifest-path")
        except ValueError:
            return None
        if i + 1 >= len(self.args):
            return None
        return pathlib.Path(self.args[i + 1])

    def built_binary_path(self) -> pathlib.Path | None:
        """Return the binary this spec launches directly, if it launches one.

        A cargo profile build is invoked by absolute path rather than
        through cargo, so the agent starts the server without a build step
        in front of it. That is the shape this recognises.
        """
        if self.command == "cargo" or "/" not in self.command:
            return None
        return pathlib.Path(self.command)

    def local_repo_path(self) -> pathlib.Path | None:
        """Return the repo a spec points into, whichever cargo shape it uses."""
        manifest = self.cargo_manifest_path()
        if manifest is not None:
            # crates/<name>/Cargo.toml -> repo root
            return manifest.parent.parent.parent
        binary = self.built_binary_path()
        if binary is not None and binary.parent.parent.name == "target":
            return binary.parent.parent.parent
        return None

    def cargo_profile(self) -> str | None:
        """Return ``debug`` or ``release`` for a profile build."""
        binary = self.built_binary_path()
        if binary is None or binary.parent.parent.name != "target":
            return None
        return binary.parent.name


@dataclasses.dataclass
class SwapEntry:
    """One CLI's bookkeeping for a swap, written to the state file."""

    config_path: str
    backup_path: str
    server: str
    action: t.Literal["replaced", "added"]
    #: ``YYYYMMDDHHMMSS`` registration timestamp, human-readable for
    #: anyone inspecting ``state.json`` directly. Sort order is enforced
    #: separately via :attr:`seq_no` so this field stays purely
    #: descriptive.
    swapped_at: str
    #: Monotonic registration counter — the primary LIFO sort key for
    #: ``cmd_revert``. ``cmd_use_local`` computes the next value as
    #: ``max(existing seq_nos, default=-1) + 1`` so it strictly
    #: increases per swap regardless of wall-clock collisions or dict
    #: iteration order. Same explicit-counter pattern CPython's
    #: ``Lib/sched.py`` uses to break ties on ``Event(time, priority,
    #: sequence, …)``.
    seq_no: int
    #: Exact destination changed by the swap.
    target_path: str | None = None
    version: int = 0
    original_mode: int | None = None
    expected_config: dict[str, t.Any] | None = None
    expected_backup: dict[str, t.Any] | None = None


class FileState(t.NamedTuple):
    """Stable bytes and identity for one regular file."""

    device: int
    inode: int
    mode: int
    size: int
    modified_ns: int
    data: bytes


class DirectoryState(t.NamedTuple):
    """Logical and resolved identity for one existing directory."""

    logical: pathlib.Path
    physical: pathlib.Path
    symlink: bool
    link_text: str | None
    link_device: int
    link_inode: int
    link_mode: int
    device: int
    inode: int
    mode: int


class ConfigState(t.NamedTuple):
    """One config's logical topology and resolved regular-file state."""

    info: CLIInfo
    parent: DirectoryState
    symlink: bool
    link_text: str | None
    link_device: int
    link_inode: int
    link_mode: int
    target: pathlib.Path
    file: FileState


class RecoveryState(t.NamedTuple):
    """One regular recovery path, which may be absent."""

    path: pathlib.Path
    parent: DirectoryState
    physical: pathlib.Path
    file: FileState | None


@dataclasses.dataclass
class SwapPlan:
    """One fully rendered config change awaiting backup and commit."""

    cli: CLIName
    scope: Scope
    label: str
    info: CLIInfo
    config: ConfigState
    backup: RecoveryState
    new_bytes: bytes
    action: t.Literal["replaced", "added"]
    server: str
    swapped_at: str
    prior: SwapEntry | None
    owner_key: tuple[CLIName, Scope] | None
    backup_is_new: bool
    backup_rewrites: tuple[BackupRewrite, ...]
    backup_note: str = ""


class BackupRewrite(t.NamedTuple):
    """A newer recovery layer adjusted after an older layer changes."""

    key: tuple[CLIName, Scope]
    revealed_key: tuple[CLIName, Scope]
    backup: RecoveryState
    new_bytes: bytes


class StagedBackupRewrite(t.NamedTuple):
    """One backup rewrite and its rollback stages."""

    plan: BackupRewrite
    output: pathlib.Path
    recovery: pathlib.Path


class StagedUse(t.NamedTuple):
    """A planned use update and its task-owned stages."""

    plan: SwapPlan
    output: pathlib.Path
    recovery: pathlib.Path
    backup: pathlib.Path | None
    backup_rewrites: tuple[StagedBackupRewrite, ...]


class RevertPlan(t.NamedTuple):
    """One selected recovery layer to restore."""

    config: ConfigState
    key: tuple[CLIName, Scope]
    backup: RecoveryState
    restore_bytes: bytes
    restore_mode: int


class StagedRevert(t.NamedTuple):
    """A planned revert and exact rollback artifacts."""

    plan: RevertPlan
    restored: pathlib.Path
    recovery: pathlib.Path
    backup_recovery: pathlib.Path


class OwnedConfig(t.NamedTuple):
    """Authenticated active config and recovery stack from persisted state."""

    config: ConfigState
    chain: list[tuple[CLIName, Scope]]
    backups: dict[tuple[CLIName, Scope], RecoveryState]


class SwapStateError(RuntimeError):
    """Swap state is unsafe to use for a mutating operation."""


def _raise_changed(message: str) -> t.NoReturn:
    raise RuntimeError(message)


def _raise_state(message: str) -> t.NoReturn:
    raise SwapStateError(message)


def _file_state(path: pathlib.Path) -> FileState:
    before = path.stat()
    if not stat.S_ISREG(before.st_mode):
        raise ValueError(f"{path} is not a regular file")
    data = path.read_bytes()
    after = path.stat()
    before_key = (
        before.st_dev,
        before.st_ino,
        before.st_mode,
        before.st_size,
        before.st_mtime_ns,
    )
    after_key = (
        after.st_dev,
        after.st_ino,
        after.st_mode,
        after.st_size,
        after.st_mtime_ns,
    )
    if before_key != after_key:
        _raise_changed(f"{path} changed while it was read")
    return FileState(
        after.st_dev,
        after.st_ino,
        stat.S_IMODE(after.st_mode),
        after.st_size,
        after.st_mtime_ns,
        data,
    )


def _directory_state(path: pathlib.Path) -> DirectoryState:
    logical = path.lstat()
    symlink = stat.S_ISLNK(logical.st_mode)
    if not symlink and not stat.S_ISDIR(logical.st_mode):
        raise ValueError(f"{path} is not a directory or directory symlink")
    physical = path.resolve(strict=True)
    details = physical.stat()
    if not stat.S_ISDIR(details.st_mode):
        raise ValueError(f"{path} is not a directory")
    return DirectoryState(
        path,
        physical,
        symlink,
        str(path.readlink()) if symlink else None,
        logical.st_dev,
        logical.st_ino,
        logical.st_mode,
        details.st_dev,
        details.st_ino,
        stat.S_IMODE(details.st_mode),
    )


def _config_state(info: CLIInfo) -> ConfigState:
    parent = _directory_state(info.config_path.parent)
    details = info.config_path.lstat()
    symlink = stat.S_ISLNK(details.st_mode)
    if not symlink and not stat.S_ISREG(details.st_mode):
        raise ValueError(f"{info.config_path} is not a regular file or symlink")
    target = info.config_path.resolve(strict=True)
    file = _file_state(target)
    if not symlink and (details.st_dev, details.st_ino) != (file.device, file.inode):
        _raise_changed(f"{info.config_path} changed while it was resolved")
    return ConfigState(
        info,
        parent,
        symlink,
        str(info.config_path.readlink()) if symlink else None,
        details.st_dev,
        details.st_ino,
        details.st_mode,
        target,
        file,
    )


def _recovery_state(path: pathlib.Path, *, required: bool = False) -> RecoveryState:
    parent = _directory_state(path.parent)
    physical = parent.physical / path.name
    if not os.path.lexists(path):
        if required:
            raise FileNotFoundError(path)
        return RecoveryState(path, parent, physical, None)
    details = path.lstat()
    if stat.S_ISLNK(details.st_mode) or not stat.S_ISREG(details.st_mode):
        raise ValueError(f"{path} is not a regular file")
    if path.resolve(strict=True) != physical:
        _raise_changed(f"{path} changed while it was resolved")
    file = _file_state(physical)
    if (details.st_dev, details.st_ino) != (file.device, file.inode):
        _raise_changed(f"{path} changed while it was resolved")
    return RecoveryState(path, parent, physical, file)


def _verify_directory(expected: DirectoryState) -> None:
    if _directory_state(expected.logical) != expected:
        _raise_changed(f"{expected.logical} changed")


def _verify_config(config: ConfigState, expected: FileState) -> None:
    _verify_directory(config.parent)
    details = config.info.config_path.lstat()
    if config.symlink:
        if (
            not stat.S_ISLNK(details.st_mode)
            or str(config.info.config_path.readlink()) != config.link_text
            or (details.st_dev, details.st_ino, details.st_mode)
            != (config.link_device, config.link_inode, config.link_mode)
        ):
            _raise_changed(f"{config.info.config_path} symlink changed")
    elif not stat.S_ISREG(details.st_mode):
        _raise_changed(f"{config.info.config_path} topology changed")
    if config.info.config_path.resolve(strict=True) != config.target:
        _raise_changed(f"{config.info.config_path} target changed")
    current = _file_state(config.target)
    if current != expected:
        _raise_changed(f"{config.info.config_path} identity, mode, or bytes changed")
    if not config.symlink and (details.st_dev, details.st_ino) != (
        current.device,
        current.inode,
    ):
        _raise_changed(f"{config.info.config_path} logical identity changed")


def _verify_recovery(recovery: RecoveryState, expected: FileState | None) -> None:
    _verify_directory(recovery.parent)
    if expected is None:
        if os.path.lexists(recovery.path):
            _raise_changed(f"{recovery.path} appeared")
        return
    current = _recovery_state(recovery.path, required=True)
    if current.parent != recovery.parent or current.physical != recovery.physical:
        _raise_changed(f"{recovery.path} target changed")
    if current.file != expected:
        _raise_changed(f"{recovery.path} identity, mode, or bytes changed")


def _file_document(file: FileState) -> dict[str, t.Any]:
    return {
        "device": file.device,
        "inode": file.inode,
        "mode": file.mode,
        "sha256": hashlib.sha256(file.data).hexdigest(),
        "size": file.size,
    }


def _directory_document(directory: DirectoryState) -> dict[str, t.Any]:
    return {
        "device": directory.device,
        "inode": directory.inode,
        "link_device": directory.link_device if directory.symlink else None,
        "link_inode": directory.link_inode if directory.symlink else None,
        "link_mode": directory.link_mode if directory.symlink else None,
        "link_text": directory.link_text,
        "logical": str(directory.logical),
        "mode": directory.mode,
        "physical": str(directory.physical),
        "symlink": directory.symlink,
    }


def _config_document(config: ConfigState, file: FileState) -> dict[str, t.Any]:
    return {
        "file": _file_document(file),
        "link_device": config.link_device if config.symlink else None,
        "link_inode": config.link_inode if config.symlink else None,
        "link_mode": config.link_mode if config.symlink else None,
        "link_text": config.link_text,
        "logical": str(config.info.config_path),
        "parent": _directory_document(config.parent),
        "symlink": config.symlink,
        "target": str(config.target),
    }


def _recovery_document(recovery: RecoveryState, file: FileState) -> dict[str, t.Any]:
    return {
        "file": _file_document(file),
        "parent": _directory_document(recovery.parent),
        "path": str(recovery.path),
        "target": str(recovery.physical),
    }


def _same_typed(left: t.Any, right: t.Any) -> bool:
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        return set(left) == set(right) and all(
            _same_typed(left[key], right[key]) for key in left
        )
    if isinstance(left, list):
        return len(left) == len(right) and all(
            _same_typed(one, two) for one, two in zip(left, right, strict=True)
        )
    return bool(left == right)


def _object(value: t.Any, keys: set[str], label: str) -> dict[str, t.Any]:
    if not isinstance(value, dict) or set(value) != keys:
        raise ValueError(f"{label} has unknown or missing fields")
    return value


def _integer(value: t.Any, label: str, *, maximum: int | None = None) -> int:
    if type(value) is not int or value < 0 or (maximum is not None and value > maximum):
        raise ValueError(f"{label} is invalid")
    return value


def _absolute(value: t.Any, label: str) -> str:
    if type(value) is not str or not pathlib.Path(value).is_absolute():
        raise ValueError(f"{label} is invalid")
    return value


def _validate_file_document(value: t.Any, label: str) -> dict[str, t.Any]:
    document = _object(value, {"device", "inode", "mode", "sha256", "size"}, label)
    _integer(document["device"], f"{label} device")
    _integer(document["inode"], f"{label} inode")
    _integer(document["mode"], f"{label} mode", maximum=0o7777)
    _integer(document["size"], f"{label} size")
    digest = document["sha256"]
    if type(digest) is not str or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
        raise ValueError(f"{label} digest is invalid")
    return document


def _validate_directory_document(value: t.Any, label: str) -> dict[str, t.Any]:
    document = _object(
        value,
        {
            "device",
            "inode",
            "link_device",
            "link_inode",
            "link_mode",
            "link_text",
            "logical",
            "mode",
            "physical",
            "symlink",
        },
        label,
    )
    _integer(document["device"], f"{label} device")
    _integer(document["inode"], f"{label} inode")
    _integer(document["mode"], f"{label} mode", maximum=0o7777)
    _absolute(document["logical"], f"{label} logical path")
    _absolute(document["physical"], f"{label} physical path")
    if type(document["symlink"]) is not bool:
        raise ValueError(f"{label} symlink flag is invalid")
    link_values = (
        document["link_device"],
        document["link_inode"],
        document["link_mode"],
        document["link_text"],
    )
    if document["symlink"]:
        _integer(link_values[0], f"{label} link device")
        _integer(link_values[1], f"{label} link inode")
        _integer(link_values[2], f"{label} link mode")
        if type(link_values[3]) is not str:
            raise ValueError(f"{label} link text is invalid")
    elif any(item is not None for item in link_values):
        raise ValueError(f"{label} link metadata is invalid")
    return document


def _validate_config_document(value: t.Any) -> dict[str, t.Any]:
    document = _object(
        value,
        {
            "file",
            "link_device",
            "link_inode",
            "link_mode",
            "link_text",
            "logical",
            "parent",
            "symlink",
            "target",
        },
        "recovery config",
    )
    _validate_file_document(document["file"], "recovery config file")
    _validate_directory_document(document["parent"], "recovery config parent")
    _absolute(document["logical"], "recovery config logical path")
    _absolute(document["target"], "recovery config target")
    if type(document["symlink"]) is not bool:
        raise ValueError("recovery config symlink flag is invalid")
    links = (
        document["link_device"],
        document["link_inode"],
        document["link_mode"],
        document["link_text"],
    )
    if document["symlink"]:
        _integer(links[0], "recovery config link device")
        _integer(links[1], "recovery config link inode")
        _integer(links[2], "recovery config link mode")
        if type(links[3]) is not str:
            raise ValueError("recovery config link text is invalid")
    elif any(item is not None for item in links):
        raise ValueError("recovery config link metadata is invalid")
    return document


def _validate_recovery_document(value: t.Any) -> dict[str, t.Any]:
    document = _object(value, {"file", "parent", "path", "target"}, "backup")
    _validate_file_document(document["file"], "backup file")
    _validate_directory_document(document["parent"], "backup parent")
    _absolute(document["path"], "backup path")
    _absolute(document["target"], "backup target")
    return document


def _state_entry_from_document(
    value: dict[str, t.Any], *, allow_legacy: bool
) -> SwapEntry:
    if value.get("version", 0) == 0:
        if not allow_legacy:
            raise ValueError("legacy swap state cannot authorize a mutation")
        required = {
            "config_path",
            "backup_path",
            "server",
            "action",
            "swapped_at",
            "seq_no",
        }
        optional = {
            "target_path",
            "version",
            "original_mode",
            "expected_config",
            "expected_backup",
        }
        if not required <= set(value) or set(value) - required - optional:
            raise ValueError("legacy recovery entry has unknown or missing fields")
        if value.get("version", 0) != 0 or any(
            value.get(key) is not None
            for key in ("original_mode", "expected_config", "expected_backup")
        ):
            raise ValueError("legacy recovery entry has invalid ownership fields")
        return SwapEntry(
            config_path=_absolute(value["config_path"], "config path"),
            backup_path=_absolute(value["backup_path"], "backup path"),
            server=str(value["server"]),
            action=value["action"],
            swapped_at=str(value["swapped_at"]),
            seq_no=int(value["seq_no"]),
            target_path=value.get("target_path"),
        )

    document = _object(
        value,
        {
            "action",
            "backup_path",
            "config_path",
            "expected_backup",
            "expected_config",
            "original_mode",
            "seq_no",
            "server",
            "swapped_at",
            "target_path",
            "version",
        },
        "recovery entry",
    )
    if document["version"] != STATE_VERSION:
        raise ValueError("recovery entry version is unsupported")
    config_path = _absolute(document["config_path"], "config path")
    backup_path = _absolute(document["backup_path"], "backup path")
    target_path = _absolute(document["target_path"], "target path")
    expected_config = _validate_config_document(document["expected_config"])
    expected_backup = _validate_recovery_document(document["expected_backup"])
    if (
        expected_config["logical"] != config_path
        or expected_config["target"] != target_path
    ):
        raise ValueError("recovery config paths disagree")
    if expected_backup["path"] != backup_path:
        raise ValueError("recovery backup paths disagree")
    action = document["action"]
    if action not in ("replaced", "added"):
        raise ValueError("recovery action is invalid")
    if type(document["server"]) is not str or not document["server"]:
        raise ValueError("recovery server is invalid")
    if type(document["swapped_at"]) is not str:
        raise ValueError("recovery timestamp is invalid")
    return SwapEntry(
        config_path=config_path,
        backup_path=backup_path,
        server=document["server"],
        action=action,
        swapped_at=document["swapped_at"],
        seq_no=_integer(document["seq_no"], "recovery sequence"),
        target_path=target_path,
        version=STATE_VERSION,
        original_mode=_integer(
            document["original_mode"], "original mode", maximum=0o7777
        ),
        expected_config=expected_config,
        expected_backup=expected_backup,
    )


def _state_checksum(version: int, entries: dict[str, t.Any]) -> str:
    canonical = json.dumps(
        {"entries": entries, "version": version},
        sort_keys=True,
        separators=(",", ":"),
    ).encode()
    return hashlib.sha256(canonical).hexdigest()


def _state_payload(
    entries: dict[tuple[CLIName, Scope], SwapEntry],
) -> dict[str, t.Any]:
    raw_entries = {
        _state_key(cli, scope): dataclasses.asdict(entry)
        for (cli, scope), entry in entries.items()
    }
    return {
        "version": STATE_VERSION,
        "checksum": _state_checksum(STATE_VERSION, raw_entries),
        "entries": raw_entries,
    }


def _state_bytes(entries: dict[tuple[CLIName, Scope], SwapEntry]) -> bytes:
    data = (json.dumps(_state_payload(entries), indent=2) + "\n").encode()
    if len(data) > STATE_MAX_BYTES:
        raise ValueError(f"recovery state exceeds {STATE_MAX_BYTES} bytes")
    return data


def _stage_file(
    directory: pathlib.Path,
    logical_name: str,
    role: str,
    data: bytes,
    mode: int,
) -> pathlib.Path:
    descriptor, temporary_name = tempfile.mkstemp(
        prefix=f".{logical_name}.mcp-swap-{role}-", dir=str(directory)
    )
    temporary = pathlib.Path(temporary_name)
    try:
        with os.fdopen(descriptor, "wb") as stream:
            os.fchmod(stream.fileno(), mode)
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
    except Exception:
        temporary.unlink(missing_ok=True)
        raise
    return temporary


def _apply_replace(
    staged: pathlib.Path, destination: pathlib.Path
) -> tuple[FileState, Exception | None]:
    expected = _file_state(staged)
    delayed_error: Exception | None = None
    try:
        staged.replace(destination)
    except Exception as exc:
        try:
            committed = _file_state(destination)
        except (OSError, RuntimeError, ValueError):
            raise exc
        if committed != expected:
            raise
        delayed_error = exc
    else:
        committed = _file_state(destination)
    if committed != expected:
        _raise_changed(f"atomic replacement of {destination} was not exact")
    return committed, delayed_error


def _publish_absent(
    staged: pathlib.Path, destination: pathlib.Path
) -> tuple[FileState, Exception | None]:
    """Move a stage only when no unowned destination has appeared."""
    if os.path.lexists(destination):
        _raise_changed(f"{destination} appeared before publication")
    return _apply_replace(staged, destination)


def _raise_if_delayed(error: Exception | None) -> None:
    if error is not None:
        raise error


def _cleanup_owned(
    owned: set[pathlib.Path], preserve: set[pathlib.Path] | None = None
) -> list[str]:
    retained = preserve or set()
    errors: list[str] = []
    for path in sorted(owned - retained, key=str):
        try:
            path.unlink(missing_ok=True)
        except OSError as exc:
            errors.append(f"could not remove task-owned stage {path}: {exc}")
    return errors


def _assert_destination_feasible(path: pathlib.Path) -> None:
    candidate = path.parent
    while not os.path.lexists(candidate):
        parent = candidate.parent
        if parent == candidate:
            raise OSError(f"no existing parent for {path}")
        candidate = parent
    if not candidate.is_dir():
        raise NotADirectoryError(candidate)
    if not os.access(candidate, os.W_OK | os.X_OK):
        raise PermissionError(f"destination is not writable: {path}")


# ---------------------------------------------------------------------------
# JSONC — comments and trailing commas, edited without reserializing
# ---------------------------------------------------------------------------
#
# tomlkit gives TOML a format-preserving round trip; JSONC has no
# equivalent on PyPI that is safe to depend on here. ``json-five`` was
# measured first and rejected: it raises on ``"C:\\x"`` and silently
# decodes the literal six characters ``\u0041`` to ``"A"`` — both valid
# JSON that stdlib reads correctly, and the second is exactly the silent
# rewrite this script exists to avoid.
#
# So values come from stdlib ``json`` (correct escape semantics) and
# edits are applied as text splices located by an offset-preserving
# scanner. Every byte outside a replaced value survives untouched, which
# is the same technique opencode's own config writer uses via
# ``jsonc-parser``'s ``modify()``.

_JSON_WS = " \t\n\r"

#: Longest inline rendering of a scalar list before it is broken across
#: lines. A swapped ``command`` array is the common case and reads
#: better on one line, which is how these configs are written by hand.
_INLINE_WIDTH = 88


def _jsonc_blank_comments(text: str) -> str:
    """Replace comment bytes with spaces, preserving every offset.

    Scanning rather than matching a regex is the whole point: ``//``
    inside a URL and ``/*`` inside a Windows path are string content, not
    comments, and only a scanner that tracks string state can tell them
    apart. Offsets are preserved so a span found in the blanked text
    addresses the same bytes in the original.
    """
    out = list(text)
    i, n = 0, len(text)
    in_string = False
    while i < n:
        char = text[i]
        if in_string:
            if char == "\\":
                i += 2
                continue
            if char == '"':
                in_string = False
            i += 1
        elif char == '"':
            in_string = True
            i += 1
        elif char == "/" and i + 1 < n and text[i + 1] == "/":
            while i < n and text[i] != "\n":
                out[i] = " "
                i += 1
        elif char == "/" and i + 1 < n and text[i + 1] == "*":
            end = text.find("*/", i + 2)
            end = n if end == -1 else end + 2
            for j in range(i, end):
                if out[j] != "\n":
                    out[j] = " "
            i = end
        else:
            i += 1
    return "".join(out)


def _jsonc_blank_trailing_commas(blanked: str) -> str:
    """Blank trailing commas so stdlib :func:`json.loads` accepts the text."""
    out = list(blanked)
    i, n = 0, len(blanked)
    in_string = False
    last_comma = -1
    while i < n:
        char = blanked[i]
        if in_string:
            if char == "\\":
                i += 2
                continue
            if char == '"':
                in_string = False
            i += 1
            continue
        if char == '"':
            in_string = True
            last_comma = -1
        elif char == ",":
            last_comma = i
        elif char in "}]":
            if last_comma != -1:
                out[last_comma] = " "
            last_comma = -1
        elif char not in _JSON_WS:
            last_comma = -1
        i += 1
    return "".join(out)


def _jsonc_loads(text: str) -> t.Any:
    """Parse JSONC text into plain Python objects."""
    if not text.strip():
        return {}
    return json.loads(_jsonc_blank_trailing_commas(_jsonc_blank_comments(text)))


class _JsoncScanner:
    """Locate value spans inside comment-blanked JSON text."""

    def __init__(self, text: str) -> None:
        self.text = text
        self.pos = 0

    def skip_ws(self) -> None:
        """Advance past insignificant whitespace."""
        while self.pos < len(self.text) and self.text[self.pos] in _JSON_WS:
            self.pos += 1

    def read_string(self) -> str:
        """Consume one string token and return its raw text, quotes included."""
        start = self.pos
        self.pos += 1
        while self.pos < len(self.text):
            char = self.text[self.pos]
            if char == "\\":
                self.pos += 2
                continue
            self.pos += 1
            if char == '"':
                break
        return self.text[start : self.pos]

    def read_value(self) -> tuple[int, int]:
        """Consume one value and return its ``(start, end)`` span."""
        self.skip_ws()
        start = self.pos
        char = self.text[self.pos]
        if char == '"':
            self.read_string()
        elif char in "{[":
            self._read_container()
        else:
            while (
                self.pos < len(self.text)
                and self.text[self.pos] not in ",}]"
                and self.text[self.pos] not in _JSON_WS
            ):
                self.pos += 1
        return start, self.pos

    def _read_container(self) -> None:
        self.pos += 1
        depth = 1
        while self.pos < len(self.text) and depth:
            char = self.text[self.pos]
            if char == '"':
                self.read_string()
                continue
            if char in "{[":
                depth += 1
            elif char in "}]":
                depth -= 1
            self.pos += 1

    def read_members(self, obj_start: int) -> list[_JsoncMember]:
        """Enumerate an object's members. ``obj_start`` indexes its ``{``."""
        self.pos = obj_start + 1
        found: list[_JsoncMember] = []
        while True:
            self.skip_ws()
            if self.pos >= len(self.text) or self.text[self.pos] == "}":
                return found
            if self.text[self.pos] == ",":
                self.pos += 1
                continue
            member_start = self.pos
            raw_key = self.read_string()
            self.skip_ws()
            self.pos += 1  # the ':'
            value_start, value_end = self.read_value()
            found.append(
                _JsoncMember(
                    key=json.loads(raw_key),
                    start=member_start,
                    end=value_end,
                    value_start=value_start,
                    value_end=value_end,
                )
            )


class _JsoncMember(t.NamedTuple):
    """One ``"key": value`` pair located inside a JSONC document.

    Attributes
    ----------
    key : str
        The decoded member name.
    start : int
        Offset of the opening quote of the key.
    end : int
        Offset just past the value — the end of the whole member.
    value_start : int
        Offset of the first byte of the value.
    value_end : int
        Offset just past the last byte of the value.
    """

    key: str
    start: int
    end: int
    value_start: int
    value_end: int


def _jsonc_render(value: t.Any, depth: int, *, ensure_ascii: bool) -> str:
    """Render ``value`` as JSON text indented for nesting ``depth``."""
    pad = "  " * depth
    if isinstance(value, list) and all(
        isinstance(item, (str, int, float, bool)) or item is None for item in value
    ):
        inline = json.dumps(value, ensure_ascii=ensure_ascii)
        if len(inline) + len(pad) <= _INLINE_WIDTH:
            return inline
    return json.dumps(value, indent=2, ensure_ascii=ensure_ascii).replace(
        "\n", "\n" + pad
    )


def _jsonc_object_span(blanked: str, path: tuple[str, ...]) -> tuple[int, int] | None:
    """Return the span of the object reached by ``path``, or ``None``."""
    scanner = _JsoncScanner(blanked)
    scanner.skip_ws()
    if scanner.pos >= len(blanked) or blanked[scanner.pos] != "{":
        return None
    cursor = scanner.pos
    for key in path:
        match = next(
            (m for m in _JsoncScanner(blanked).read_members(cursor) if m.key == key),
            None,
        )
        if match is None or blanked[match.value_start] != "{":
            return None
        cursor = match.value_start
    tail = _JsoncScanner(blanked)
    tail.pos = cursor
    return tail.read_value()


def _jsonc_next_edit(
    text: str,
    data: t.Mapping[str, t.Any],
    path: tuple[str, ...],
    *,
    ensure_ascii: bool,
) -> tuple[int, int, str] | None:
    """Find the one next splice that brings ``path`` closer to ``data``."""
    blanked = _jsonc_blank_comments(text)
    span = _jsonc_object_span(blanked, path)
    if span is None:
        return None
    obj_start, obj_end = span
    members = _JsoncScanner(blanked).read_members(obj_start)
    by_key = {member.key: member for member in members}
    depth = len(path) + 1
    pad = "  " * depth

    for key, value in data.items():
        member = by_key.get(key)
        if member is None:
            body = _jsonc_render(value, depth, ensure_ascii=ensure_ascii)
            # Escape the key like any other value: written raw, a backslash
            # or quote in a server name emits text that cannot be parsed
            # back, so the member is never found and the merge re-inserts
            # it until the pass ceiling, holding the swap lock throughout.
            name = json.dumps(key, ensure_ascii=ensure_ascii)
            if members:
                tail = members[-1].end
                return tail, tail, f",\n{pad}{name}: {body}"
            if blanked[obj_start + 1 : obj_end - 1].strip():
                return None
            # Blanking hid any comment the object holds, so measure the
            # interior in the original text and splice after it, not over it.
            interior = text[obj_start + 1 : obj_end - 1]
            anchor = obj_start + 1 + len(interior.rstrip())
            closing = "  " * (depth - 1)
            return anchor, obj_end - 1, f"\n{pad}{name}: {body}\n{closing}"
        current = json.loads(
            _jsonc_blank_trailing_commas(blanked[member.value_start : member.value_end])
        )
        if isinstance(value, dict) and isinstance(current, dict):
            nested = _jsonc_next_edit(
                text, value, (*path, key), ensure_ascii=ensure_ascii
            )
            if nested is not None:
                return nested
        elif current != value:
            return (
                member.value_start,
                member.value_end,
                _jsonc_render(value, depth, ensure_ascii=ensure_ascii),
            )

    for index, member in enumerate(members):
        if member.key in data:
            continue
        # Exactly one delimiter leaves with the member: the comma before
        # it, or, for the first member which has none, the comma after.
        if index:
            return members[index - 1].end, member.end, ""
        # Read that comma out of the blanked text -- one inside a comment
        # is not a delimiter, and a real one behind a comment still is.
        trailing = blanked[member.end : obj_end]
        drop_to = member.end
        if trailing.lstrip(_JSON_WS).startswith(","):
            drop_to += trailing.index(",") + 1
        return obj_start + 1, drop_to, ""
    return None


def _jsonc_merge(text: str, data: t.Mapping[str, t.Any], *, ensure_ascii: bool) -> str:
    """Reconcile ``data`` into ``text``, rewriting only members that differ.

    Applies one splice at a time and rescans, so offsets are always
    computed against current text rather than patched up after the fact.
    Config files are small enough that the extra passes do not matter and
    the invariant is worth far more than the cycles.
    """
    if not text.strip():
        return json.dumps(dict(data), indent=2, ensure_ascii=ensure_ascii) + "\n"
    # One splice per member, plus slack; a config that needs more than
    # this has a pathology worth surfacing rather than looping on.
    for _ in range(10_000):
        edit = _jsonc_next_edit(text, data, (), ensure_ascii=ensure_ascii)
        if edit is None:
            return text
        start, end, replacement = edit
        text = text[:start] + replacement + text[end:]
    msg = "JSONC merge did not converge"
    raise RuntimeError(msg)


# ---------------------------------------------------------------------------
# Config IO — per format
# ---------------------------------------------------------------------------


def _parse_config_bytes(info: CLIInfo, raw: bytes) -> t.Any:
    """Parse JSON, JSONC or TOML config bytes into an editable structure.

    Empty JSON files are treated as empty objects so first-run MCP configs can
    be seeded with their initial server entry.
    """
    if info.fmt == "jsonc":
        return _jsonc_loads(raw.decode())
    if info.fmt == "json":
        text = raw.decode().strip()
        return json.loads(text) if text else {}
    return tomlkit.parse(raw.decode())


def load_config(info: CLIInfo) -> t.Any:
    """Read and parse one CLI config."""
    return _parse_config_bytes(info, info.config_path.read_bytes())


def _json_trailer(original: bytes) -> str:
    """Return the newline a rewritten JSON config should end with.

    Claude writes ``~/.claude.json`` without a trailing newline, so
    appending one unconditionally grows the file by a byte on every swap
    and shows as a diff hunk in a region the swap never touched. Empty
    bytes mean a file being seeded, which gets the conventional newline.
    """
    if not original:
        return "\n"
    return "\n" if original.endswith(b"\n") else ""


def dump_config_bytes(info: CLIInfo, config: t.Any, *, original: bytes) -> bytes:
    """Serialize an edited config back to bytes in its original format.

    ``original`` is the file's pre-edit bytes, or empty when seeding a
    new one. The parsed structure does not record the byte-level
    conventions of the file it came from, so they are carried over from
    the source instead. Required rather than defaulted: a caller that
    omitted it would silently start rewriting regions it never touched,
    which is the defect this parameter exists to prevent. tomlkit
    preserves those conventions itself; only the JSON writer needs it.
    """
    # Dispatched on the exact format rather than "not json": a third
    # format reaching the TOML writer by fall-through would silently
    # write TOML bytes into a JSON file.
    if info.fmt == "toml":
        return tomlkit.dumps(config).encode()
    if info.fmt == "jsonc":
        # The merge derives its output from the original text, so the
        # file's own trailing-newline convention carries over untouched
        # and needs no _json_trailer fixup.
        source = original.decode()
        try:
            return _jsonc_merge(source, config, ensure_ascii=False).encode()
        except UnicodeEncodeError:
            return _jsonc_merge(source, config, ensure_ascii=True).encode()
    trailer = _json_trailer(original)
    # ensure_ascii would re-escape every non-ASCII character in the file,
    # including config text the swap never read.
    text = json.dumps(config, indent=2, ensure_ascii=False) + trailer
    try:
        return text.encode()
    except UnicodeEncodeError:
        # A lone surrogate — a JS writer slicing a string mid-pair — has no
        # UTF-8 encoding. Escaping the document is then the only form that
        # can be written at all.
        return (json.dumps(config, indent=2) + trailer).encode()


def atomic_write(path: pathlib.Path, data: bytes) -> None:
    """Write bytes to ``path`` without replacing a symlinked config.

    Parameters
    ----------
    path : pathlib.Path
        Destination path. A symlink resolves to its final target so the
        write preserves every link in the chain.
    data : bytes
        Bytes to write atomically.
    """
    target = path.resolve() if path.is_symlink() else path
    target.parent.mkdir(parents=True, exist_ok=True)
    mode = stat.S_IMODE(target.stat().st_mode) if target.exists() else None
    fd, tmp_name = tempfile.mkstemp(prefix=target.name + ".", dir=str(target.parent))
    tmp = pathlib.Path(tmp_name)
    try:
        with os.fdopen(fd, "wb") as fh:
            if mode is not None:
                os.fchmod(fh.fileno(), mode)
            fh.write(data)
        tmp.replace(target)
    except Exception:
        tmp.unlink(missing_ok=True)
        raise


def write_new_backup(base: pathlib.Path, data: bytes) -> pathlib.Path:
    """Write ``data`` to ``base``, or to ``base-1`` / ``base-2`` / … if taken.

    A backup is the only copy of the config as it stood before a swap, so
    clobbering one is unrecoverable data loss. The timestamp embedded in
    ``base`` has one-second granularity, which is not fine enough on its
    own: two swaps inside the same second derive the same path. Creation
    goes through ``O_CREAT | O_EXCL`` so the check and the claim are one
    atomic step and an existing file can never be truncated — the same
    exclusive-create discipline CPython's ``tempfile`` uses to hand out
    unique names.

    Returns the path actually written.
    """
    base.parent.mkdir(parents=True, exist_ok=True)
    candidate = base
    attempt = 0
    while True:
        try:
            fd = os.open(candidate, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        except FileExistsError:
            attempt += 1
            candidate = base.with_name(f"{base.name}-{attempt}")
            continue
        with os.fdopen(fd, "wb") as fh:
            fh.write(data)
        return candidate


def _next_backup_path(base: pathlib.Path, reserved: set[pathlib.Path]) -> pathlib.Path:
    """Return the first unclaimed backup path without creating it."""
    candidate = base
    attempt = 0
    while candidate in reserved or os.path.lexists(candidate):
        attempt += 1
        candidate = base.with_name(f"{base.name}-{attempt}")
    return candidate


def _check_backup_destination(path: pathlib.Path) -> None:
    """Reject a backup destination whose existing parent is not writable."""
    parent = path.parent
    if not parent.is_dir():
        msg = f"backup directory is not a directory: {parent}"
        raise OSError(msg)
    if not os.access(parent, os.W_OK | os.X_OK):
        msg = f"backup directory is not writable: {parent}"
        raise OSError(msg)


# ---------------------------------------------------------------------------
# Per-CLI get / set / delete (the only CLI-specific logic)
# ---------------------------------------------------------------------------


@t.overload
def _claude_project_node(
    config: dict[str, t.Any],
    repo: pathlib.Path,
    *,
    create: t.Literal[True],
) -> dict[str, t.Any]: ...


@t.overload
def _claude_project_node(
    config: dict[str, t.Any],
    repo: pathlib.Path,
    *,
    create: t.Literal[False],
) -> dict[str, t.Any] | None: ...


def _claude_project_node(
    config: dict[str, t.Any], repo: pathlib.Path, *, create: bool
) -> dict[str, t.Any] | None:
    """Return (or create) the ``projects.<abs-repo>`` node Claude keys per-project.

    With ``create=True``, the node is unconditionally created if missing
    and the return type is statically narrowed to ``dict[str, t.Any]``;
    callers can drop runtime ``assert node is not None`` defensiveness.
    With ``create=False``, the absence of the node is a real return value
    and the type stays ``dict[str, t.Any] | None``.

    Raises ``RuntimeError`` if Claude's config layout is not the
    expected ``projects.<abs>.mcpServers`` mapping shape — the layout
    is undocumented Claude Code internal state, so a clear error before
    the atomic write beats a silent partial mutation that the backup
    defense would be asked to recover from.
    """
    key = str(repo.resolve())
    projects_node = config.get("projects")
    if projects_node is not None and not isinstance(projects_node, dict):
        msg = (
            "Claude config layout appears to have changed; expected "
            f"'projects' to be a mapping but got "
            f"{type(projects_node).__name__}"
        )
        raise RuntimeError(msg)
    projects = (
        config.setdefault("projects", {}) if create else config.get("projects", {})
    )
    raw_node = projects.get(key)
    node: dict[str, t.Any] | None = None
    if isinstance(raw_node, dict):
        node = raw_node
    elif raw_node is not None:
        msg = (
            "Claude config layout appears to have changed; expected "
            f"'projects[{key!r}]' to be a mapping but got "
            f"{type(raw_node).__name__}"
        )
        raise RuntimeError(msg)
    if node is None and create:
        node = {"allowedTools": [], "mcpContextUris": [], "mcpServers": {}, "env": {}}
        projects[key] = node
    return node


@t.overload
def _claude_user_servers(
    config: dict[str, t.Any], *, create: t.Literal[True]
) -> dict[str, t.Any]: ...


@t.overload
def _claude_user_servers(
    config: dict[str, t.Any], *, create: t.Literal[False]
) -> dict[str, t.Any] | None: ...


def _claude_user_servers(
    config: dict[str, t.Any], *, create: bool
) -> dict[str, t.Any] | None:
    """Return (or create) the top-level ``mcpServers`` dict — Claude user scope.

    Mirrors :func:`_claude_project_node` for the user-scope path so the
    shape guard is centralised once and reused across read / write /
    delete instead of duplicated at each call site (or worse, missing
    on read and delete the way the inline write-side guard left them).
    Same reasoning applies as for the project-scope helper: Claude's
    config shape is undocumented internal state, so a clear
    ``RuntimeError`` before the atomic write beats an opaque
    ``AttributeError`` from ``.setdefault()`` on a non-dict.

    With ``create=True`` the dict is initialised when missing and the
    return type narrows to ``dict[str, t.Any]``. With ``create=False``
    a missing key returns ``None``.
    """
    raw = config.get("mcpServers")
    existing: dict[str, t.Any] | None = None
    if isinstance(raw, dict):
        existing = raw
    elif raw is not None:
        msg = (
            "Claude config layout appears to have changed; expected "
            f"'mcpServers' to be a mapping but got "
            f"{type(raw).__name__}"
        )
        raise RuntimeError(msg)
    if existing is None and create:
        existing = {}
        config["mcpServers"] = existing
    return existing


@t.overload
def _server_map(
    info: CLIInfo, config: t.Any, *, create: t.Literal[True]
) -> dict[str, t.Any]: ...


@t.overload
def _server_map(
    info: CLIInfo, config: t.Any, *, create: t.Literal[False]
) -> dict[str, t.Any] | None: ...


def _server_map(
    info: CLIInfo, config: t.Any, *, create: bool
) -> dict[str, t.Any] | None:
    """Walk ``info.container`` to the mapping holding this CLI's entries.

    Returns ``None`` when the path is absent and ``create`` is false.
    Intermediate levels are created on demand so a nested container needs
    no special case; TOML gets tomlkit tables so the written document
    keeps its formatting.

    Raises
    ------
    RuntimeError
        A key along the path holds something other than a mapping.
        Reported rather than overwritten — a swap must never discard
        config it cannot interpret.
    """
    node: dict[str, t.Any] = config
    for depth, key in enumerate(info.container):
        child = node.get(key)
        if child is None:
            if not create:
                return None
            child = tomlkit.table() if info.fmt == "toml" else {}
            node[key] = child
        elif not isinstance(child, dict):
            path = ".".join(info.container[: depth + 1])
            msg = (
                f"{info.config_path}: {path} is a {type(child).__name__}, "
                f"expected a table of server entries"
            )
            raise RuntimeError(msg)
        node = child
    return node


def _as_toml_table(entry: dict[str, t.Any]) -> tomlkit.items.Table:
    """Render one entry dict as a tomlkit table.

    Nested mappings (``env``) become sub-tables so the written document
    keeps TOML's own structure instead of an inline dict literal.
    """
    table = tomlkit.table()
    for key, value in entry.items():
        if isinstance(value, dict):
            sub = tomlkit.table()
            for sub_key, sub_value in value.items():
                sub[sub_key] = sub_value
            table[key] = sub
        else:
            table[key] = value
    return table


def get_server(
    cli: CLIName,
    config: t.Any,
    name: str,
    repo: pathlib.Path,
    *,
    scope: Scope = "project",
) -> McpServerSpec | None:
    """Fetch the MCP server entry for ``name`` from a CLI's config, if present.

    ``scope`` only affects Claude (see :data:`Scope` for the layered shape
    of ``~/.claude.json``); for Codex / Cursor / Gemini the parameter is
    accepted-but-ignored because their config has no per-project layer.
    """
    if cli == "claude":
        if scope == "user":
            servers = _claude_user_servers(config, create=False)
            entry = servers.get(name) if servers else None
        else:
            node = _claude_project_node(config, repo, create=False)
            if not node:
                return None
            entry = node.get("mcpServers", {}).get(name)
    else:
        servers = _server_map(CLIS[cli], config, create=False)
        entry = servers.get(name) if servers else None
    if entry is None:
        return None
    return _spec_from_entry(entry, info=CLIS[cli])


def set_server(
    cli: CLIName,
    config: t.Any,
    name: str,
    spec: McpServerSpec,
    repo: pathlib.Path,
    *,
    scope: Scope = "project",
) -> t.Literal["replaced", "added"]:
    """Write ``spec`` under ``name`` in a CLI's config, returning replaced/added.

    ``scope == "user"`` for Claude writes the top-level ``mcpServers``
    fallback used by every project that has no per-project override;
    ``"project"`` (the default, preserving pre-flag behaviour) writes
    under ``projects[abs(repo)].mcpServers``. The parameter is silently
    ignored for non-Claude CLIs.
    """
    if cli == "claude":
        if scope == "user":
            servers = _claude_user_servers(config, create=True)
            had = name in servers
            servers[name] = spec.to_entry_dict("claude")
            return "replaced" if had else "added"
        node = _claude_project_node(config, repo, create=True)
        servers = node.setdefault("mcpServers", {})
        had = name in servers
        servers[name] = spec.to_entry_dict("claude")
        return "replaced" if had else "added"
    info = CLIS[cli]
    if info.dialect == "opencode" and not config:
        # Seeding from nothing: opencode rewrites the file on load to add
        # this line, so writing it now avoids an immediate second edit.
        config["$schema"] = OPENCODE_SCHEMA_URL
    servers = _server_map(info, config, create=True)
    had = name in servers
    entry = spec.to_entry_dict(info.dialect)
    servers[name] = _as_toml_table(entry) if info.fmt == "toml" else entry
    return "replaced" if had else "added"


def delete_server(
    cli: CLIName,
    config: t.Any,
    name: str,
    repo: pathlib.Path,
    *,
    scope: Scope = "project",
) -> bool:
    """Remove the entry for ``name`` from a CLI's config; return whether it existed.

    See :func:`set_server` for the meaning of ``scope`` — the parameter
    is honoured for Claude and ignored for the other CLIs.
    """
    if cli == "claude":
        if scope == "user":
            servers = _claude_user_servers(config, create=False)
            if servers is not None and name in servers:
                del servers[name]
                return True
            return False
        node = _claude_project_node(config, repo, create=False)
        if not node:
            return False
        servers = node.get("mcpServers", {})
        return servers.pop(name, None) is not None
    servers = _server_map(CLIS[cli], config, create=False)
    if servers is None or name not in servers:
        return False
    del servers[name]
    return True


def _spec_from_entry(entry: t.Any, *, info: CLIInfo) -> McpServerSpec:
    """Convert a raw config entry (dict or tomlkit Table) into an McpServerSpec.

    Every dialect is normalised down to the portable scalar-command
    shape, so the helpers that reason about a spec —
    :meth:`McpServerSpec.local_repo_path`, :meth:`McpServerSpec.cargo_profile`,
    ``_points_at`` — stay dialect-agnostic. Skipping this is not a
    cosmetic loss: an unsplit array command makes the "already local, no
    change" check miss, and every run rewrites a config it did not need
    to touch.
    """
    # tomlkit items quack like dicts/lists; coerce to plain Python for our spec.
    if info.fmt == "toml":
        entry = (
            tomlkit.items.Table.unwrap(entry)
            if isinstance(entry, tomlkit.items.Table)
            else dict(entry)
        )
    if info.dialect == "opencode":
        raw_command = entry.get("command", [])
        argv = (
            [str(part) for part in raw_command]
            if isinstance(raw_command, (list, tuple))
            else [str(raw_command)]
        )
        command, args = (argv[0], argv[1:]) if argv else ("", [])
        raw_env = entry.get("environment") or {}
    else:
        command = str(entry.get("command", ""))
        raw_args = entry.get("args", [])
        args = [str(a) for a in raw_args] if raw_args else []
        raw_env = entry.get("env") or {}
    env = {str(k): str(v) for k, v in dict(raw_env).items()}
    return McpServerSpec(command=command, args=args, env=env)


# ---------------------------------------------------------------------------
# Repo metadata
# ---------------------------------------------------------------------------


ALL_SOURCES = ("debug", "release", "run", "path", "published")
Source = t.Literal["debug", "release", "run", "path", "published"]

DEFAULT_CRATE = "tmux-mcp"


def resolve_repo_meta(
    repo: pathlib.Path, crate: str = DEFAULT_CRATE
) -> tuple[str, str]:
    """Derive (server_name, binary_name) from the crate's Cargo.toml.

    The server name is the registration slug used as the config-file key
    (``mcpServers.<slug>`` in JSON, ``[mcp_servers.<slug>]`` in TOML):
    the package name with a trailing ``-mcp`` stripped, so ``tmux-mcp``
    registers as ``tmux`` and matches what the README tells people to
    type.

    The binary name is what cargo builds into ``target/<profile>/``. A
    crate may declare it under ``[[bin]]``; otherwise cargo names it
    after the package, and a ``src/bin/<name>.rs`` is discovered under
    its own stem.
    """
    manifest = repo / "crates" / crate / "Cargo.toml"
    if not manifest.is_file():
        manifest = repo / "Cargo.toml"
    if not manifest.is_file():
        msg = f"no Cargo.toml for {crate} under {repo}"
        raise RuntimeError(msg)

    doc = tomlkit.parse(manifest.read_text())
    package = doc.get("package")
    if package is None:
        msg = f"{manifest} has no [package] table"
        raise RuntimeError(msg)
    name = str(package["name"])
    server = name.removesuffix("-mcp")

    bins = doc.get("bin") or []
    if bins:
        binary = str(bins[0]["name"])
    else:
        candidates = sorted((manifest.parent / "src" / "bin").glob("*.rs"))
        binary = candidates[0].stem if candidates else name
    return server, binary


def manifest_path(repo: pathlib.Path, crate: str = DEFAULT_CRATE) -> pathlib.Path:
    """Return the crate manifest ``cargo`` should be pointed at."""
    scoped = repo / "crates" / crate / "Cargo.toml"
    return scoped if scoped.is_file() else repo / "Cargo.toml"


def build_profile_spec(repo: pathlib.Path, binary: str, profile: str) -> McpServerSpec:
    """Point an agent straight at a compiled binary in ``target/<profile>``.

    The agent launches the binary itself, so nothing runs in front of it:
    startup is a process spawn rather than a cargo invocation that may
    decide to rebuild while a client is waiting for the handshake.
    """
    return McpServerSpec(command=str(repo.resolve() / "target" / profile / binary))


def build_run_spec(
    repo: pathlib.Path, binary: str, crate: str = DEFAULT_CRATE
) -> McpServerSpec:
    """Launch through ``cargo run``, rebuilding on every start.

    This is the shape to use while editing: the next agent session picks
    up the current source with no build step to remember. It costs a
    rebuild check on each launch, and the first launch after a change can
    be slow enough that a client with a short handshake timeout gives up,
    which is why it is not the default.

    Cargo writes its progress to stderr, leaving stdout to carry the
    protocol -- ``preflight`` proves that rather than assuming it.
    """
    return McpServerSpec(
        command="cargo",
        args=[
            "run",
            "--quiet",
            "--manifest-path",
            str(manifest_path(repo, crate).resolve()),
            "--bin",
            binary,
            "--",
        ],
    )


def build_path_spec(binary_path: pathlib.Path) -> McpServerSpec:
    """Point at a binary the caller names, wherever it came from."""
    return McpServerSpec(command=str(binary_path.resolve()))


def published_root(version: str, binary: str) -> pathlib.Path:
    """Where a published release is installed so it cannot shadow others.

    Each version gets its own root, so swapping between releases does not
    reinstall over the previous one and reverting leaves it available.
    """
    return _xdg_state_home() / "tmux-mcp-dev" / "releases" / f"{binary}-{version}"


def build_published_spec(version: str, binary: str) -> McpServerSpec:
    """Point at a crates.io release installed under its own root."""
    return McpServerSpec(command=str(published_root(version, binary) / "bin" / binary))


def install_published(
    crate: str, version: str, binary: str, *, locked: bool = True
) -> pathlib.Path:
    """Install a crates.io release, returning the binary path.

    Skips the install when that exact version is already present, so
    repeated swaps between releases cost one download each rather than
    one per swap.
    """
    root = published_root(version, binary)
    target = root / "bin" / binary
    if target.is_file():
        return target
    argv = ["cargo", "install", crate, "--version", version, "--root", str(root)]
    if locked:
        argv.append("--locked")
    subprocess.run(argv, check=True)
    return target


def build_source_spec(
    source: Source,
    *,
    repo: pathlib.Path,
    binary: str,
    crate: str = DEFAULT_CRATE,
    version: str | None = None,
    binary_path: pathlib.Path | None = None,
) -> McpServerSpec:
    """Build the spec for one source kind."""
    if source == "debug":
        return build_profile_spec(repo, binary, "debug")
    if source == "release":
        return build_profile_spec(repo, binary, "release")
    if source == "run":
        return build_run_spec(repo, binary, crate)
    if source == "path":
        if binary_path is None:
            msg = "--source path needs --bin"
            raise RuntimeError(msg)
        return build_path_spec(binary_path)
    if source == "published":
        if version is None:
            msg = "--source published needs --version"
            raise RuntimeError(msg)
        return build_published_spec(version, binary)
    msg = f"unknown source {source!r}"
    raise RuntimeError(msg)


def cargo_build(
    repo: pathlib.Path, binary: str, profile: str, crate: str = DEFAULT_CRATE
) -> None:
    """Build the binary a profile spec points at.

    Writing a config that names a binary which does not exist yet leaves
    every agent failing to start a server, and the error surfaces inside
    the agent rather than here.
    """
    argv = [
        "cargo",
        "build",
        "--manifest-path",
        str(manifest_path(repo, crate).resolve()),
        "--bin",
        binary,
    ]
    if profile == "release":
        argv.append("--release")
    subprocess.run(argv, check=True)


def _run_text(argv: list[str], cwd: pathlib.Path | None = None) -> str:
    """Run ``argv`` and return stdout, raising on a non-zero exit."""
    return subprocess.run(
        argv,
        cwd=None if cwd is None else str(cwd),
        capture_output=True,
        text=True,
        check=True,
    ).stdout


_INITIALIZE_FRAME = (
    json.dumps(
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "mcp_swap-preflight", "version": "1"},
            },
        }
    )
    + "\n"
)


def preflight_spec(spec: McpServerSpec, *, timeout: float = 300.0) -> str | None:
    """Launch ``spec`` and complete one MCP ``initialize`` round trip.

    Returns ``None`` when the server answered, otherwise a reason to
    show the operator. A pull-request spec resolves its dependencies at
    launch time, inside whichever agent starts it, so an unresolvable
    ref would otherwise land in every config and surface later as an
    opaque startup failure in each one.

    Closing stdin after the frame lets a well-behaved stdio server exit
    on its own, which keeps this free of signal handling.
    """
    try:
        proc = subprocess.Popen(
            [spec.command, *spec.args],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={**os.environ, **spec.env},
            text=True,
        )
    except OSError as exc:
        return f"could not launch {spec.command}: {exc}"

    try:
        out, err = proc.communicate(_INITIALIZE_FRAME, timeout=timeout)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.communicate()
        return f"no MCP response within {timeout:.0f}s"

    for line in out.splitlines():
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(message, dict) and message.get("id") == 1 and "result" in message:
            return None

    tail = "\n".join(err.strip().splitlines()[-3:])
    return tail or "server exited without answering initialize"


# ---------------------------------------------------------------------------
# State file
# ---------------------------------------------------------------------------


def _decode_state_data(
    data: bytes, *, strict: bool
) -> dict[tuple[CLIName, Scope], SwapEntry]:
    if len(data) > STATE_MAX_BYTES:
        raise SwapStateError(
            f"swap state exceeds {STATE_MAX_BYTES} bytes: {STATE_FILE}"
        )
    try:
        raw = json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise SwapStateError(f"swap state unreadable ({STATE_FILE}): {exc}") from exc
    if not isinstance(raw, dict):
        _raise_state(f"swap state has invalid shape: {STATE_FILE}")

    legacy = set(raw) == {"entries"}
    if legacy:
        if strict:
            raise SwapStateError(
                f"legacy swap state cannot authorize a mutation: {STATE_FILE}"
            )
    else:
        if set(raw) != {"checksum", "entries", "version"}:
            _raise_state(f"swap state has unknown or missing fields: {STATE_FILE}")
        if type(raw["version"]) is not int or raw["version"] != STATE_VERSION:
            _raise_state(f"swap state version is unsupported: {STATE_FILE}")
        if type(raw["checksum"]) is not str:
            _raise_state(f"swap state checksum is invalid: {STATE_FILE}")
        expected = _state_checksum(raw["version"], raw["entries"])
        if raw["checksum"] != expected:
            _raise_state(f"swap state checksum does not match: {STATE_FILE}")

    entries = raw.get("entries")
    if not isinstance(entries, dict):
        _raise_state(f"swap state has invalid entries: {STATE_FILE}")
    out: dict[tuple[CLIName, Scope], SwapEntry] = {}
    for key, value in entries.items():
        parsed = _parse_state_key(key)
        entry = (
            _parse_state_entry(value, allow_legacy=not strict)
            if isinstance(value, dict)
            else None
        )
        if parsed is None or entry is None:
            if strict:
                raise SwapStateError(
                    f"swap state has invalid entry {key!r}: {STATE_FILE}"
                )
            continue
        cli, _scope = parsed
        if strict and entry.config_path != str(CLIS[cli].config_path):
            raise SwapStateError(
                f"swap state config path changed for {key!r}: {STATE_FILE}"
            )
        out[parsed] = entry
    return out


def load_state(*, strict: bool = False) -> dict[tuple[CLIName, Scope], SwapEntry]:
    """Read bounded, versioned recovery state without trusting partial records."""
    if not os.path.lexists(STATE_FILE):
        return {}
    try:
        details = STATE_FILE.lstat()
        if stat.S_ISLNK(details.st_mode) or not stat.S_ISREG(details.st_mode):
            _raise_state(f"swap state is not a regular file: {STATE_FILE}")
        file = _file_state(STATE_FILE)
        if strict and file.mode != 0o600:
            _raise_state(f"swap state mode is not 0600: {STATE_FILE}")
        return _decode_state_data(file.data, strict=strict)
    except (OSError, RuntimeError, ValueError, SwapStateError) as exc:
        message = str(exc)
        print(message, file=sys.stderr)
        if strict:
            raise SwapStateError(message) from exc
        return {}


@contextlib.contextmanager
def _state_lock() -> t.Iterator[None]:
    """Serialize config mutations that share the swap state file."""
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    fd = os.open(STATE_DIR / "state.lock", os.O_RDWR | os.O_CREAT, 0o600)
    with os.fdopen(fd, "a+b") as lock_file:
        fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX)
        yield


def save_state(entries: dict[tuple[CLIName, Scope], SwapEntry]) -> None:
    """Write the swap-state file atomically."""
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    atomic_write(STATE_FILE, _state_bytes(entries))


# ---------------------------------------------------------------------------
# Detection
# ---------------------------------------------------------------------------


@dataclasses.dataclass
class Presence:
    """Detection outcome for a CLI: binary on PATH and config file present."""

    cli: CLIName
    binary_found: bool
    config_found: bool

    @property
    def present(self) -> bool:
        """Return True only when both the binary and the config file were found."""
        return self.binary_found and self.config_found


def detect_clis() -> list[Presence]:
    """Probe all supported CLIs and return their detection results."""
    return [
        Presence(
            cli=info.name,
            binary_found=shutil.which(info.binary) is not None,
            config_found=info.config_path.exists(),
        )
        for info in CLIS.values()
    ]


def present_clis() -> list[CLIName]:
    """Return the list of CLIs that have both a binary and a config present."""
    return [p.cli for p in detect_clis() if p.present]


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------


def cmd_detect(args: argparse.Namespace) -> int:
    """Print detection results for every supported CLI."""
    for p in detect_clis():
        flag = "yes" if p.present else " no"
        extra = []
        if not p.binary_found:
            extra.append("binary missing")
        if not p.config_found:
            extra.append(f"config missing: {CLIS[p.cli].config_path}")
        if p.cli == "pi" and not PI_ADAPTER_DIR.is_dir():
            extra.append(PI_ADAPTER_HINT)
        suffix = f"  ({', '.join(extra)})" if extra else ""
        print(f"  [{flag}] {p.cli:<{_CLI_COLUMN}}{suffix}")
    return 0


def cmd_status(args: argparse.Namespace) -> int:
    """Print the current MCP server entry per detected CLI.

    For Claude, prints separate lines for the user-level fallback
    (``[claude:user]``) and the per-project override
    (``[claude:project]``) when both exist; if only one exists, only
    that line shows. ``args.scope`` (when set) restricts Claude output
    to the matching layer only. Other CLIs print a single line as
    ``[<cli>]`` since their config has no scope concept and ignore
    ``args.scope``.
    """
    repo = pathlib.Path(args.repo).resolve()
    server = args.server or resolve_repo_meta(repo)[0]
    scope_filter: Scope | None = args.scope
    for cli in args.cli or present_clis():
        info = CLIS[cli]
        if not info.config_path.exists():
            print(f"[{cli}] (no config at {info.config_path})")
            continue
        # Wrap the read + shape-guarded queries in try/except RuntimeError
        # so a malformed Claude config surfaces as a clean per-CLI error
        # instead of aborting status output for the rest of the CLIs.
        try:
            config = load_config(info)
            if cli == "claude":
                # Lazy reads: skip the get_server call entirely for the
                # filtered-out scope so a malformed projects node doesn't
                # raise when the user only asked about user scope.
                user_spec = (
                    get_server(cli, config, server, repo, scope="user")
                    if scope_filter in (None, "user")
                    else None
                )
                project_spec = (
                    get_server(cli, config, server, repo, scope="project")
                    if scope_filter in (None, "project")
                    else None
                )
                shown = False
                if user_spec is not None:
                    tag = _describe_spec(user_spec, repo)
                    print(
                        f"[claude:user] {server} = {user_spec.command} "
                        f"{' '.join(user_spec.args)}  ({tag})"
                    )
                    shown = True
                if project_spec is not None:
                    tag = _describe_spec(project_spec, repo)
                    print(
                        f"[claude:project] {server} = {project_spec.command} "
                        f"{' '.join(project_spec.args)}  ({tag})"
                    )
                    shown = True
                if not shown:
                    label = f"claude:{scope_filter}" if scope_filter else "claude"
                    print(f"[{label}] no entry for {server!r}")
            else:
                spec = get_server(cli, config, server, repo)
                if spec is None:
                    print(f"[{cli}] no entry for {server!r}")
                    continue
                tag = _describe_spec(spec, repo)
                print(
                    f"[{cli}] {server} = {spec.command} {' '.join(spec.args)}  ({tag})"
                )
        except (RuntimeError, ValueError, OSError) as exc:
            print(f"[{cli}] {exc}", file=sys.stderr)
            continue
    return 0


def _describe_spec(spec: McpServerSpec, repo: pathlib.Path) -> str:
    """Return a short label saying where a configured server comes from."""
    manifest = spec.cargo_manifest_path()
    if manifest is not None:
        local = spec.local_repo_path()
        here = local is not None and local.resolve() == repo.resolve()
        return "cargo run: this repo" if here else f"cargo run: {manifest}"

    binary = spec.built_binary_path()
    if binary is not None:
        profile = spec.cargo_profile()
        local = spec.local_repo_path()
        if profile and local and local.resolve() == repo.resolve():
            return f"{profile} build: this repo"
        releases = _xdg_state_home() / "tmux-mcp-dev" / "releases"
        try:
            relative = binary.relative_to(releases)
        except ValueError:
            return f"binary: {binary}"
        else:
            # releases/<binary>-<version>/bin/<binary>
            return f"crates.io: {relative.parts[0].rsplit('-', 1)[-1]}"

    if "/" not in spec.command:
        return f"on PATH: {spec.command}"
    return "other"


def _points_at(
    current: McpServerSpec, target: McpServerSpec, repo: pathlib.Path
) -> bool:
    """Return True when ``current`` already runs what ``target`` describes."""
    return current.command == target.command and current.args == target.args


def _strict_state_snapshot() -> tuple[
    dict[tuple[CLIName, Scope], SwapEntry], RecoveryState
]:
    state_file = _recovery_state(STATE_FILE)
    if state_file.file is None:
        return {}, state_file
    if state_file.file.mode != 0o600:
        _raise_state(f"swap state mode is not 0600: {STATE_FILE}")
    return _decode_state_data(state_file.file.data, strict=True), state_file


def _entry_config(entry: SwapEntry) -> dict[str, t.Any]:
    if entry.version != STATE_VERSION or entry.expected_config is None:
        _raise_state("recovery entry has no recorded config state")
    return entry.expected_config


def _entry_backup(entry: SwapEntry) -> dict[str, t.Any]:
    if entry.version != STATE_VERSION or entry.expected_backup is None:
        _raise_state("recovery entry has no recorded backup state")
    return entry.expected_backup


def _route_document(config: dict[str, t.Any]) -> dict[str, t.Any]:
    return {key: value for key, value in config.items() if key != "file"}


def _validate_state_sequences(
    state: dict[tuple[CLIName, Scope], SwapEntry],
) -> None:
    sequences = [entry.seq_no for entry in state.values()]
    if len(sequences) != len(set(sequences)):
        _raise_state("swap state contains duplicate sequence numbers")


def _validate_config_chain(
    config: ConfigState,
    state: dict[tuple[CLIName, Scope], SwapEntry],
) -> tuple[
    list[tuple[CLIName, Scope]],
    dict[tuple[CLIName, Scope], RecoveryState],
]:
    keys = [
        key
        for key, entry in state.items()
        if entry.config_path == str(config.info.config_path)
    ]
    keys.sort(key=lambda key: state[key].seq_no, reverse=True)
    if not keys:
        return [], {}

    expected_current = _entry_config(state[keys[0]])
    if not _same_typed(expected_current, _config_document(config, config.file)):
        raise SwapStateError(
            f"config no longer matches recovery state: {config.info.config_path}"
        )

    backups: dict[tuple[CLIName, Scope], RecoveryState] = {}
    route = _route_document(expected_current)
    for key in keys:
        entry = state[key]
        expected_config = _entry_config(entry)
        if not _same_typed(_route_document(expected_config), route):
            raise SwapStateError(
                f"recovery route changed for {config.info.config_path}"
            )
        backup = _recovery_state(pathlib.Path(entry.backup_path), required=True)
        expected_backup = _entry_backup(entry)
        if not _same_typed(
            expected_backup,
            _recovery_document(backup, t.cast(FileState, backup.file)),
        ):
            raise SwapStateError(
                f"backup no longer matches recovery state: {backup.path}"
            )
        backups[key] = backup

    for newer, older in itertools.pairwise(keys):
        newer_entry = state[newer]
        older_file = _entry_config(state[older])["file"]
        backup_file = _entry_backup(newer_entry)["file"]
        if (
            backup_file["sha256"] != older_file["sha256"]
            or backup_file["size"] != older_file["size"]
            or newer_entry.original_mode != older_file["mode"]
        ):
            raise SwapStateError(
                f"recovery layers are not a valid stack: {config.info.config_path}"
            )
    return keys, backups


def _snapshot_owned_state(
    state: dict[tuple[CLIName, Scope], SwapEntry],
) -> dict[str, OwnedConfig]:
    """Authenticate every active config and backup owned by swap state."""
    groups: dict[str, list[tuple[CLIName, Scope]]] = {}
    for key, entry in state.items():
        groups.setdefault(entry.config_path, []).append(key)

    snapshots: dict[str, OwnedConfig] = {}
    for logical, keys in groups.items():
        clients = {cli for cli, _scope in keys}
        if len(clients) != 1:
            _raise_state(f"multiple clients claim recovery for {logical}")
        cli = next(iter(clients))
        config = _config_state(CLIS[cli])
        chain, backups = _validate_config_chain(config, state)
        if set(chain) != set(keys):
            _raise_state(f"recovery stack is incomplete for {logical}")
        snapshots[logical] = OwnedConfig(config, chain, backups)
    return snapshots


def _reject_transaction_aliases(
    configs: list[tuple[str, ConfigState]],
    recoveries: list[tuple[str, RecoveryState]],
) -> None:
    paths: dict[pathlib.Path, str] = {STATE_FILE.resolve(): "swap state"}
    inodes: dict[tuple[int, int], str] = {}
    if os.path.lexists(STATE_FILE):
        state_file = _recovery_state(STATE_FILE, required=True)
        inodes[
            (
                t.cast(FileState, state_file.file).device,
                t.cast(FileState, state_file.file).inode,
            )
        ] = "swap state"

    config_paths: dict[pathlib.Path, str] = {}
    config_inodes: dict[tuple[int, int], str] = {}
    for label, config in configs:
        identity = (config.file.device, config.file.inode)
        other = config_paths.get(config.target) or config_inodes.get(identity)
        if other is not None:
            _raise_state(f"duplicate physical config target for {other} and {label}")
        owner = paths.get(config.target) or inodes.get(identity)
        if owner is not None:
            _raise_state(
                f"duplicate transaction destination for {owner} and {label} config"
            )
        config_paths[config.target] = label
        config_inodes[identity] = label
        paths[config.target] = f"{label} config"
        inodes[identity] = f"{label} config"

    for label, recovery in recoveries:
        identity = (
            None
            if recovery.file is None
            else (recovery.file.device, recovery.file.inode)
        )
        owner = paths.get(recovery.physical)
        if owner is None and identity is not None:
            owner = inodes.get(identity)
        if owner is not None:
            _raise_state(f"duplicate transaction destination for {owner} and {label}")
        paths[recovery.physical] = label
        if identity is not None:
            inodes[identity] = label


def _plan_use_local(
    args: argparse.Namespace,
    repo: pathlib.Path,
    server: str,
    spec: McpServerSpec,
    extra_env: dict[str, str],
    state: dict[tuple[CLIName, Scope], SwapEntry],
) -> tuple[list[SwapPlan], int]:
    targets: list[CLIName] = list(dict.fromkeys(args.cli or present_clis()))
    if not targets:
        print("no CLIs detected — nothing to do", file=sys.stderr)
        return [], 1

    _validate_state_sequences(state)
    owned_state = _snapshot_owned_state(state)
    timestamp = time.strftime("%Y%m%d%H%M%S")
    plans: list[SwapPlan] = []
    selected_configs: list[tuple[str, ConfigState]] = []
    had_error = 0
    for cli in targets:
        scope = _normalize_scope(cli, args.scope)
        label = f"{cli}:{scope}" if cli == "claude" else cli
        info = CLIS[cli]
        if not os.path.lexists(info.config_path):
            print(f"[{label}] skip — config not found at {info.config_path}")
            had_error = 1
            continue
        try:
            owned = owned_state.get(str(info.config_path))
            config_state = owned.config if owned is not None else _config_state(info)
            selected_configs.append((label, config_state))
            chain, backups = (
                (owned.chain, owned.backups) if owned is not None else ([], {})
            )
            config = _parse_config_bytes(info, config_state.file.data)
            current = get_server(cli, config, server, repo, scope=scope)
            if (
                current
                and _points_at(current, spec, repo)
                and all(
                    current.env.get(key) == value for key, value in extra_env.items()
                )
            ):
                where = _describe_spec(spec, repo)
                print(f"[{label}] already {where} — no change")
                continue
            base_env = dict(current.env) if current else {}
            base_env.update(extra_env)
            cli_spec = dataclasses.replace(spec, env=base_env) if base_env else spec
            action = set_server(cli, config, server, cli_spec, repo, scope=scope)
            new_bytes = dump_config_bytes(info, config, original=config_state.file.data)
            key = (cli, scope)
            prior = state.get(key)
            backup_rewrites: list[BackupRewrite] = []
            if prior is None:
                suffix = f"{BACKUP_SUFFIX_PREFIX}{timestamp}"
                if cli == "claude":
                    suffix += f"-{scope}"
                backup_path = _next_backup_path(
                    info.config_path.with_suffix(info.config_path.suffix + suffix),
                    set(),
                )
                backup = _recovery_state(backup_path)
                backup_is_new = True
                owner_key = None
            else:
                backup = backups[key]
                backup_is_new = False
                owner_key = chain[0]
                selected_at = chain.index(key)
                for index, newer_key in enumerate(chain[:selected_at]):
                    newer_backup = backups[newer_key]
                    newer_file = t.cast(FileState, newer_backup.file)
                    historical = _parse_config_bytes(info, newer_file.data)
                    set_server(
                        cli,
                        historical,
                        server,
                        cli_spec,
                        repo,
                        scope=scope,
                    )
                    rewritten = dump_config_bytes(
                        info, historical, original=newer_file.data
                    )
                    backup_rewrites.append(
                        BackupRewrite(
                            newer_key,
                            chain[index + 1],
                            newer_backup,
                            rewritten,
                        )
                    )
            _check_backup_destination(backup.path)
            _assert_destination_feasible(backup.physical)
            _assert_destination_feasible(config_state.target)
            for rewrite in backup_rewrites:
                _assert_destination_feasible(rewrite.backup.physical)
            plans.append(
                SwapPlan(
                    cli,
                    scope,
                    label,
                    info,
                    config_state,
                    backup,
                    new_bytes,
                    action,
                    server,
                    timestamp,
                    prior,
                    owner_key,
                    backup_is_new,
                    tuple(backup_rewrites),
                )
            )
        except (RuntimeError, ValueError, OSError, SwapStateError) as exc:
            print(f"[{label}] {exc}", file=sys.stderr)
            had_error = 1
    if had_error:
        return [], had_error

    selected_logicals = {
        str(config.info.config_path) for _label, config in selected_configs
    }
    all_configs = selected_configs + [
        (f"{logical} owned config", owned.config)
        for logical, owned in owned_state.items()
        if logical not in selected_logicals
    ]
    recoveries = [
        (f"{_state_key(*key)} backup", backup)
        for owned in owned_state.values()
        for key, backup in owned.backups.items()
    ]
    recoveries.extend(
        (f"{plan.label} backup", plan.backup) for plan in plans if plan.prior is None
    )
    _reject_transaction_aliases(all_configs, recoveries)
    _assert_destination_feasible(STATE_FILE)
    return plans, 0


def _stage_use_local(
    plans: list[SwapPlan],
    state: dict[tuple[CLIName, Scope], SwapEntry],
    state_file: RecoveryState,
) -> tuple[
    list[StagedUse],
    dict[tuple[CLIName, Scope], SwapEntry],
    pathlib.Path,
    pathlib.Path | None,
    set[pathlib.Path],
]:
    owned: set[pathlib.Path] = set()
    staged: list[StagedUse] = []
    next_state = dict(state)
    next_seq = max((entry.seq_no for entry in state.values()), default=-1) + 1
    try:
        for plan in plans:
            output = _stage_file(
                plan.config.target.parent,
                plan.info.config_path.name,
                "output",
                plan.new_bytes,
                plan.config.file.mode,
            )
            owned.add(output)
            recovery = _stage_file(
                plan.config.target.parent,
                plan.info.config_path.name,
                "recovery",
                plan.config.file.data,
                plan.config.file.mode,
            )
            owned.add(recovery)
            backup_stage = None
            if plan.backup_is_new:
                backup_stage = _stage_file(
                    plan.backup.parent.physical,
                    plan.backup.path.name,
                    "backup",
                    plan.config.file.data,
                    0o600,
                )
                owned.add(backup_stage)
                backup_file = _file_state(backup_stage)
                plan.backup_note = f"backup: {plan.backup.path}"
            else:
                backup_file = t.cast(FileState, plan.backup.file)
                plan.backup_note = f"pre-swap backup kept: {plan.backup.path}"

            staged_rewrites: list[StagedBackupRewrite] = []
            for rewrite in plan.backup_rewrites:
                rewrite_file = t.cast(FileState, rewrite.backup.file)
                rewrite_output = _stage_file(
                    rewrite.backup.parent.physical,
                    rewrite.backup.path.name,
                    "backup-output",
                    rewrite.new_bytes,
                    rewrite_file.mode,
                )
                owned.add(rewrite_output)
                rewrite_recovery = _stage_file(
                    rewrite.backup.parent.physical,
                    rewrite.backup.path.name,
                    "backup-recovery",
                    rewrite_file.data,
                    rewrite_file.mode,
                )
                owned.add(rewrite_recovery)
                staged_rewrites.append(
                    StagedBackupRewrite(rewrite, rewrite_output, rewrite_recovery)
                )
                output_state = _file_state(rewrite_output)
                backup_owner = next_state[rewrite.key]
                next_state[rewrite.key] = dataclasses.replace(
                    backup_owner,
                    expected_backup=_recovery_document(rewrite.backup, output_state),
                )
                revealed_owner = next_state[rewrite.revealed_key]
                revealed_mode = t.cast(int, backup_owner.original_mode)
                revealed_state = output_state._replace(mode=revealed_mode)
                next_state[rewrite.revealed_key] = dataclasses.replace(
                    revealed_owner,
                    expected_config=_config_document(plan.config, revealed_state),
                )

            output_file = _file_state(output)
            if plan.prior is None:
                key = (plan.cli, plan.scope)
                next_state[key] = SwapEntry(
                    config_path=str(plan.info.config_path),
                    backup_path=str(plan.backup.path),
                    server=plan.server,
                    action=plan.action,
                    swapped_at=plan.swapped_at,
                    seq_no=next_seq,
                    target_path=str(plan.config.target),
                    version=STATE_VERSION,
                    original_mode=plan.config.file.mode,
                    expected_config=_config_document(plan.config, output_file),
                    expected_backup=_recovery_document(plan.backup, backup_file),
                )
                next_seq += 1
            else:
                owner_key = t.cast(tuple[CLIName, Scope], plan.owner_key)
                owner = next_state[owner_key]
                next_state[owner_key] = dataclasses.replace(
                    owner,
                    expected_config=_config_document(plan.config, output_file),
                )
            staged.append(
                StagedUse(
                    plan,
                    output,
                    recovery,
                    backup_stage,
                    tuple(staged_rewrites),
                )
            )

        state_stage = _stage_file(
            state_file.parent.physical,
            state_file.path.name,
            "state",
            _state_bytes(next_state),
            0o600,
        )
        owned.add(state_stage)
        state_recovery = None
        if state_file.file is not None:
            state_recovery = _stage_file(
                state_file.parent.physical,
                state_file.path.name,
                "state-recovery",
                state_file.file.data,
                state_file.file.mode,
            )
            owned.add(state_recovery)
    except Exception:
        _cleanup_owned(owned)
        raise
    return staged, next_state, state_stage, state_recovery, owned


def _restore_config_stage(item: StagedUse, committed: FileState | None) -> None:
    config = item.plan.config
    if committed is None:
        if os.path.lexists(config.target):
            _raise_changed(f"{config.target} appeared before rollback")
    else:
        _verify_config(config, committed)
    operation = _publish_absent if committed is None else _apply_replace
    restored, _delayed_error = operation(item.recovery, config.target)
    if restored != config.file:
        _raise_changed(f"{item.plan.label} config rollback changed identity")
    _verify_config(config, config.file)


def _commit_use_local(
    staged: list[StagedUse],
    next_state: dict[tuple[CLIName, Scope], SwapEntry],
    state_file: RecoveryState,
    state_stage: pathlib.Path,
    state_recovery: pathlib.Path | None,
    owned: set[pathlib.Path],
) -> int:
    committed_backups: list[tuple[StagedUse, FileState]] = []
    rewritten_backups: list[tuple[StagedBackupRewrite, FileState | None]] = []
    config_operations: list[tuple[StagedUse, FileState | None]] = []
    state_committed: FileState | None = None
    state_removed = False
    try:
        for item in staged:
            _verify_config(item.plan.config, item.plan.config.file)
            _verify_recovery(item.plan.backup, item.plan.backup.file)
            for rewrite in item.backup_rewrites:
                _verify_recovery(rewrite.plan.backup, rewrite.plan.backup.file)
        _verify_recovery(state_file, state_file.file)

        for item in staged:
            if item.backup is None:
                continue
            _verify_config(item.plan.config, item.plan.config.file)
            _verify_recovery(item.plan.backup, None)
            committed, delayed_error = _publish_absent(
                item.backup, item.plan.backup.physical
            )
            owned.discard(item.backup)
            committed_backups.append((item, committed))
            _raise_if_delayed(delayed_error)

        for item in staged:
            for rewrite in item.backup_rewrites:
                backup = rewrite.plan.backup
                _verify_recovery(backup, backup.file)
                moved, delayed_error = _apply_replace(backup.physical, rewrite.recovery)
                rewritten_backups.append((rewrite, None))
                if moved != backup.file:
                    _raise_changed(f"backup recovery identity changed: {backup.path}")
                _raise_if_delayed(delayed_error)
                committed, delayed_error = _publish_absent(
                    rewrite.output, backup.physical
                )
                owned.discard(rewrite.output)
                rewritten_backups[-1] = (rewrite, committed)
                _raise_if_delayed(delayed_error)

        for item in staged:
            expected = next(
                (
                    committed
                    for committed_item, committed in committed_backups
                    if committed_item is item
                ),
                item.plan.backup.file,
            )
            _verify_config(item.plan.config, item.plan.config.file)
            _verify_recovery(item.plan.backup, expected)
            for rewrite in item.backup_rewrites:
                committed = next(
                    current
                    for current_rewrite, current in rewritten_backups
                    if current_rewrite is rewrite
                )
                _verify_recovery(rewrite.plan.backup, committed)
        _verify_recovery(state_file, state_file.file)

        if state_file.file is not None:
            recovery = t.cast(pathlib.Path, state_recovery)
            moved, delayed_error = _apply_replace(state_file.physical, recovery)
            state_removed = True
            if moved != state_file.file:
                _raise_changed("swap state recovery identity changed")
            _raise_if_delayed(delayed_error)
        state_committed, delayed_error = _publish_absent(
            state_stage, state_file.physical
        )
        owned.discard(state_stage)
        _raise_if_delayed(delayed_error)

        for item in staged:
            plan = item.plan
            _verify_config(plan.config, plan.config.file)
            current_state = _recovery_state(STATE_FILE, required=True)
            if current_state.file != state_committed:
                _raise_changed("swap state changed before config commit")
            moved, delayed_error = _apply_replace(plan.config.target, item.recovery)
            config_operations.append((item, None))
            if moved != plan.config.file:
                _raise_changed(f"{plan.label} config recovery identity changed")
            _raise_if_delayed(delayed_error)
            committed, delayed_error = _publish_absent(item.output, plan.config.target)
            owned.discard(item.output)
            config_operations[-1] = (item, committed)
            _raise_if_delayed(delayed_error)
            _verify_config(plan.config, committed)
            owner_key = (
                (plan.cli, plan.scope)
                if plan.prior is None
                else t.cast(tuple[CLIName, Scope], plan.owner_key)
            )
            if not _same_typed(
                _entry_config(next_state[owner_key]),
                _config_document(plan.config, committed),
            ):
                _raise_changed(f"{plan.label} recovery state does not own the config")
    except Exception as exc:  # noqa: BLE001
        rollback_errors: list[str] = []
        preserved: set[pathlib.Path] = set()
        for item, committed in reversed(config_operations):
            try:
                _restore_config_stage(item, committed)
                owned.discard(item.recovery)
            except Exception as rollback_exc:  # noqa: BLE001
                rollback_errors.append(f"{item.plan.label} config: {rollback_exc}")
                if item.recovery.exists():
                    preserved.add(item.recovery)

        if rollback_errors:
            if os.path.lexists(STATE_FILE):
                preserved.add(STATE_FILE)
            if state_recovery is not None and state_recovery.exists():
                preserved.add(state_recovery)
            preserved.update(item.plan.backup.path for item, _ in committed_backups)
        else:
            try:
                if state_committed is not None:
                    _verify_recovery(
                        _recovery_state(STATE_FILE, required=True), state_committed
                    )
                    if state_file.file is None:
                        STATE_FILE.unlink()
                    else:
                        recovery = t.cast(pathlib.Path, state_recovery)
                        restored, _delayed_error = _apply_replace(
                            recovery, state_file.physical
                        )
                        owned.discard(recovery)
                        if restored != state_file.file:
                            _raise_changed("swap state rollback changed identity")
                elif state_removed:
                    recovery = t.cast(pathlib.Path, state_recovery)
                    if os.path.lexists(STATE_FILE):
                        _raise_changed("swap state appeared before rollback")
                    restored, _delayed_error = _publish_absent(
                        recovery, state_file.physical
                    )
                    owned.discard(recovery)
                    if restored != state_file.file:
                        _raise_changed("swap state rollback changed identity")
                _verify_recovery(state_file, state_file.file)
            except Exception as rollback_exc:  # noqa: BLE001
                rollback_errors.append(f"recovery state: {rollback_exc}")
                if state_recovery is not None and state_recovery.exists():
                    preserved.add(state_recovery)
                preserved.update(item.plan.backup.path for item, _ in committed_backups)

        if not rollback_errors:
            for rewrite, committed in reversed(rewritten_backups):
                backup = rewrite.plan.backup
                try:
                    if committed is None:
                        if os.path.lexists(backup.path):
                            _raise_changed(f"{backup.path} appeared before rollback")
                    else:
                        _verify_recovery(backup, committed)
                    operation = _publish_absent if committed is None else _apply_replace
                    restored, _delayed_error = operation(
                        rewrite.recovery, backup.physical
                    )
                    owned.discard(rewrite.recovery)
                    if restored != backup.file:
                        _raise_changed(f"{backup.path} rollback changed identity")
                    _verify_recovery(backup, backup.file)
                except Exception as rollback_exc:  # noqa: BLE001
                    rollback_errors.append(f"backup {backup.path}: {rollback_exc}")
                    if rewrite.recovery.exists():
                        preserved.add(rewrite.recovery)
                    if os.path.lexists(backup.path):
                        preserved.add(backup.path)

        if rollback_errors:
            preserved.update(item.plan.backup.path for item, _ in committed_backups)
            preserved.update(
                rewrite.recovery
                for rewrite, _ in rewritten_backups
                if rewrite.recovery.exists()
            )

        if not rollback_errors:
            for item, committed in reversed(committed_backups):
                try:
                    _verify_recovery(
                        _recovery_state(item.plan.backup.path, required=True), committed
                    )
                    item.plan.backup.path.unlink()
                except Exception as rollback_exc:  # noqa: BLE001
                    rollback_errors.append(f"{item.plan.label} backup: {rollback_exc}")
                    preserved.add(item.plan.backup.path)

        cleanup_errors = _cleanup_owned(owned, preserved)
        details = [f"swap failed: {exc}"]
        if rollback_errors:
            details.append("rollback incomplete: " + "; ".join(rollback_errors))
        if cleanup_errors:
            details.append("cleanup incomplete: " + "; ".join(cleanup_errors))
        if preserved:
            details.append(
                "recovery artifacts: "
                + ", ".join(str(path) for path in sorted(preserved, key=str))
            )
        print("; ".join(details), file=sys.stderr)
        return 1

    cleanup_errors = _cleanup_owned(owned)
    if cleanup_errors:
        print("; ".join(cleanup_errors), file=sys.stderr)
        return 1
    for item in staged:
        print(f"[{item.plan.label}] {item.plan.action}; {item.plan.backup_note}")
    return 0


def _cmd_use_local(args: argparse.Namespace) -> int:
    """Plan, stage, and commit every selected config as one transaction."""
    repo = pathlib.Path(args.repo).resolve()
    crate = getattr(args, "crate", None) or DEFAULT_CRATE
    server, default_binary = resolve_repo_meta(repo, crate)
    server = args.server or server
    binary = args.entry or default_binary
    extra_env = dict(args.env or [])
    source: Source = getattr(args, "source", "debug")
    try:
        spec = build_source_spec(
            source,
            repo=repo,
            binary=binary,
            crate=crate,
            version=getattr(args, "version", None),
            binary_path=(
                pathlib.Path(args.bin) if getattr(args, "bin", None) else None
            ),
        )
    except RuntimeError as exc:
        print(f"{source}: {exc}", file=sys.stderr)
        return 1
    spec = dataclasses.replace(spec, env=dict(extra_env))

    hint = _naming_hint(repo, server)
    if hint:
        print(hint, file=sys.stderr)
    try:
        if args.dry_run:
            state = load_state(strict=True)
            state_file = None
        else:
            state, state_file = _strict_state_snapshot()
        plans, error = _plan_use_local(args, repo, server, spec, extra_env, state)
    except (OSError, RuntimeError, ValueError, SwapStateError) as exc:
        print(exc, file=sys.stderr)
        return 1
    if error:
        return error

    if args.dry_run:
        for plan in plans:
            print(f"--- {plan.info.config_path} (current)")
            print(f"+++ {plan.info.config_path} (proposed)")
            diff = difflib.unified_diff(
                plan.config.file.data.decode(errors="replace").splitlines(
                    keepends=True
                ),
                plan.new_bytes.decode(errors="replace").splitlines(keepends=True),
                lineterm="",
            )
            sys.stdout.writelines(diff)
        return 0

    if not plans:
        return 0

    try:
        if source in ("debug", "release") and not getattr(args, "no_build", False):
            cargo_build(repo, binary, source, crate)
        if source == "published":
            install_published(crate, args.version, binary)
    except (RuntimeError, subprocess.CalledProcessError) as exc:
        print(f"{source}: {exc}", file=sys.stderr)
        return 1

    launcher = pathlib.Path(spec.command)
    if spec.command != "cargo" and not launcher.is_file():
        print(f"{source}: {launcher} does not exist", file=sys.stderr)
        return 1
    if not args.no_preflight:
        print(f"preflight: {spec.command} {' '.join(spec.args)}", file=sys.stderr)
        failure = preflight_spec(spec)
        if failure is not None:
            print(f"preflight failed, nothing written:\n{failure}", file=sys.stderr)
            return 1

    try:
        staged, next_state, state_stage, state_recovery, owned = _stage_use_local(
            plans, state, t.cast(RecoveryState, state_file)
        )
    except (OSError, RuntimeError, ValueError) as exc:
        print(f"swap staging failed: {exc}", file=sys.stderr)
        return 1
    return _commit_use_local(
        staged,
        next_state,
        t.cast(RecoveryState, state_file),
        state_stage,
        state_recovery,
        owned,
    )


def cmd_use_local(args: argparse.Namespace) -> int:
    """Run :func:`_cmd_use_local` under the shared mutation lock."""
    if args.dry_run:
        return _cmd_use_local(args)
    try:
        with _state_lock():
            return _cmd_use_local(args)
    except SwapStateError:
        return 1
    except OSError as exc:
        print(f"swap state unavailable: {exc}", file=sys.stderr)
        return 1


def _plan_revert(
    args: argparse.Namespace,
    state: dict[tuple[CLIName, Scope], SwapEntry],
) -> tuple[list[RevertPlan], int]:
    targets = list(dict.fromkeys(args.cli or (cli for cli, _scope in state)))
    if not targets:
        print("no recorded swaps — nothing to revert", file=sys.stderr)
        return [], 1

    selected: list[tuple[CLIName, Scope]] = []
    for cli in targets:
        scopes = (
            (_normalize_scope(cli, args.scope),)
            if args.scope is not None
            else ALL_SCOPES
        )
        keys = [key for key in state if key[0] == cli and key[1] in scopes]
        if not keys:
            label = f"{cli}:{args.scope}" if args.scope and cli == "claude" else cli
            print(f"[{label}] no state entry — skip")
        selected.extend(keys)
    if not selected:
        return [], 0

    _validate_state_sequences(state)
    logical_owners: dict[str, str] = {}
    for key in selected:
        logical = state[key].config_path
        label = _state_key(*key)
        if logical in logical_owners:
            print(
                "duplicate physical config target for "
                f"{logical_owners[logical]} and {label}",
                file=sys.stderr,
            )
            return [], 1
        logical_owners[logical] = label

    plans: list[RevertPlan] = []
    try:
        owned_state = _snapshot_owned_state(state)
        for key in selected:
            cli, _scope = key
            entry = state[key]
            info = CLIS[cli]
            if str(info.config_path) != entry.config_path:
                _raise_state(f"recovery config path changed for {cli}")
            owned = owned_state[entry.config_path]
            config = owned.config
            chain = owned.chain
            backups = owned.backups
            if not chain or chain[0] != key:
                _raise_state(
                    f"selected recovery layer is not the top of {info.config_path}"
                )
            backup = backups[key]
            backup_file = t.cast(FileState, backup.file)
            if entry.original_mode is None:
                _raise_state(f"recovery mode is missing for {info.config_path}")
            _check_backup_destination(backup.path)
            _assert_destination_feasible(backup.physical)
            _assert_destination_feasible(config.target)
            plans.append(
                RevertPlan(
                    config,
                    key,
                    backup,
                    backup_file.data,
                    entry.original_mode,
                )
            )
        configs = [
            (f"{logical} owned config", owned.config)
            for logical, owned in owned_state.items()
        ]
        recoveries = [
            (f"{_state_key(*key)} backup", backup)
            for owned in owned_state.values()
            for key, backup in owned.backups.items()
        ]
        _reject_transaction_aliases(configs, recoveries)
        _assert_destination_feasible(STATE_FILE)
    except (OSError, RuntimeError, ValueError, SwapStateError) as exc:
        print(exc, file=sys.stderr)
        return [], 1
    return plans, 0


def _stage_revert(
    plans: list[RevertPlan],
    state: dict[tuple[CLIName, Scope], SwapEntry],
    state_file: RecoveryState,
) -> tuple[
    list[StagedRevert],
    dict[tuple[CLIName, Scope], SwapEntry],
    pathlib.Path | None,
    pathlib.Path,
    set[pathlib.Path],
]:
    owned: set[pathlib.Path] = set()
    staged: list[StagedRevert] = []
    next_state = dict(state)
    try:
        for plan in plans:
            restored = _stage_file(
                plan.config.target.parent,
                plan.config.info.config_path.name,
                "restore",
                plan.restore_bytes,
                plan.restore_mode,
            )
            owned.add(restored)
            recovery = _stage_file(
                plan.config.target.parent,
                plan.config.info.config_path.name,
                "recovery",
                plan.config.file.data,
                plan.config.file.mode,
            )
            owned.add(recovery)
            backup_file = t.cast(FileState, plan.backup.file)
            backup_recovery = _stage_file(
                plan.backup.parent.physical,
                plan.backup.path.name,
                "recovery",
                backup_file.data,
                backup_file.mode,
            )
            owned.add(backup_recovery)
            staged.append(StagedRevert(plan, restored, recovery, backup_recovery))
            next_state.pop(plan.key)

        for item in staged:
            remaining = [
                key
                for key, entry in next_state.items()
                if entry.config_path == str(item.plan.config.info.config_path)
            ]
            if not remaining:
                continue
            top = max(remaining, key=lambda key: next_state[key].seq_no)
            next_state[top] = dataclasses.replace(
                next_state[top],
                expected_config=_config_document(
                    item.plan.config, _file_state(item.restored)
                ),
            )

        state_stage = None
        if next_state:
            state_stage = _stage_file(
                state_file.parent.physical,
                state_file.path.name,
                "state",
                _state_bytes(next_state),
                0o600,
            )
            owned.add(state_stage)
        current_state = t.cast(FileState, state_file.file)
        state_recovery = _stage_file(
            state_file.parent.physical,
            state_file.path.name,
            "state-recovery",
            current_state.data,
            current_state.mode,
        )
        owned.add(state_recovery)
    except Exception:
        _cleanup_owned(owned)
        raise
    return staged, next_state, state_stage, state_recovery, owned


def _commit_revert(
    staged: list[StagedRevert],
    next_state: dict[tuple[CLIName, Scope], SwapEntry],
    state_file: RecoveryState,
    state_stage: pathlib.Path | None,
    state_recovery: pathlib.Path,
    owned: set[pathlib.Path],
) -> int:
    config_operations: list[tuple[StagedRevert, FileState | None]] = []
    removed_backups: list[tuple[RecoveryState, pathlib.Path]] = []
    state_committed: FileState | None = None
    state_removed = False
    try:
        for item in staged:
            _verify_config(item.plan.config, item.plan.config.file)
            _verify_recovery(item.plan.backup, item.plan.backup.file)
        _verify_recovery(state_file, state_file.file)

        for item in staged:
            plan = item.plan
            _verify_config(plan.config, plan.config.file)
            _verify_recovery(plan.backup, plan.backup.file)
            moved, delayed_error = _apply_replace(plan.config.target, item.recovery)
            config_operations.append((item, None))
            if moved != plan.config.file:
                _raise_changed("config recovery identity changed")
            _raise_if_delayed(delayed_error)
            committed, delayed_error = _publish_absent(
                item.restored, plan.config.target
            )
            owned.discard(item.restored)
            config_operations[-1] = (item, committed)
            _raise_if_delayed(delayed_error)
            _verify_config(plan.config, committed)
            remaining = [
                key
                for key, entry in next_state.items()
                if entry.config_path == str(plan.config.info.config_path)
            ]
            if remaining:
                top = max(remaining, key=lambda key: next_state[key].seq_no)
                if not _same_typed(
                    _entry_config(next_state[top]),
                    _config_document(plan.config, committed),
                ):
                    _raise_changed("remaining recovery state does not own config")

        _verify_recovery(state_file, state_file.file)
        moved, delayed_error = _apply_replace(state_file.physical, state_recovery)
        state_removed = True
        if moved != state_file.file:
            _raise_changed("swap state recovery identity changed")
        _raise_if_delayed(delayed_error)
        if state_stage is not None:
            state_committed, delayed_error = _publish_absent(
                state_stage, state_file.physical
            )
            owned.discard(state_stage)
            _raise_if_delayed(delayed_error)
        elif os.path.lexists(STATE_FILE):
            _raise_changed("swap state remained after removal")

        for item in staged:
            backup = item.plan.backup
            _verify_recovery(backup, backup.file)
            if state_committed is not None:
                current_state = _recovery_state(STATE_FILE, required=True)
                if current_state.file != state_committed:
                    _raise_changed("swap state changed before backup removal")
            elif os.path.lexists(STATE_FILE):
                _raise_changed("swap state reappeared before backup removal")
            moved, delayed_error = _apply_replace(backup.physical, item.backup_recovery)
            removed_backups.append((backup, item.backup_recovery))
            if moved != backup.file:
                _raise_changed(f"backup recovery identity changed: {backup.path}")
            _raise_if_delayed(delayed_error)
            if os.path.lexists(backup.path):
                _raise_changed(f"backup remained after removal: {backup.path}")
    except Exception as exc:  # noqa: BLE001
        rollback_errors: list[str] = []
        preserved: set[pathlib.Path] = set()
        for backup, recovery in reversed(removed_backups):
            try:
                if os.path.lexists(backup.path):
                    _raise_changed(f"{backup.path} appeared before rollback")
                restored, _delayed_error = _publish_absent(recovery, backup.physical)
                owned.discard(recovery)
                if restored != backup.file:
                    _raise_changed(f"{backup.path} rollback changed identity")
                _verify_recovery(backup, backup.file)
            except Exception as rollback_exc:  # noqa: BLE001
                rollback_errors.append(f"backup {backup.path}: {rollback_exc}")
                if recovery.exists():
                    preserved.add(recovery)

        try:
            if state_removed:
                if state_committed is not None:
                    current = _recovery_state(STATE_FILE, required=True)
                    if current.file != state_committed:
                        _raise_changed("swap state changed before rollback")
                elif os.path.lexists(STATE_FILE):
                    _raise_changed("swap state appeared before rollback")
                operation = (
                    _apply_replace if state_committed is not None else _publish_absent
                )
                restored, _delayed_error = operation(
                    state_recovery, state_file.physical
                )
                owned.discard(state_recovery)
                if restored != state_file.file:
                    _raise_changed("swap state rollback changed identity")
                _verify_recovery(state_file, state_file.file)
        except Exception as rollback_exc:  # noqa: BLE001
            rollback_errors.append(f"recovery state: {rollback_exc}")
            if state_recovery.exists():
                preserved.add(state_recovery)

        for item, committed in reversed(config_operations):
            try:
                if committed is None:
                    if os.path.lexists(item.plan.config.target):
                        _raise_changed("config appeared before rollback")
                else:
                    _verify_config(item.plan.config, committed)
                operation = _apply_replace if committed is not None else _publish_absent
                restored, _delayed_error = operation(
                    item.recovery, item.plan.config.target
                )
                owned.discard(item.recovery)
                if restored != item.plan.config.file:
                    _raise_changed("config rollback changed identity")
                _verify_config(item.plan.config, item.plan.config.file)
            except Exception as rollback_exc:  # noqa: BLE001
                label = item.plan.config.info.name
                rollback_errors.append(f"{label} config: {rollback_exc}")
                if item.recovery.exists():
                    preserved.add(item.recovery)

        cleanup_errors = _cleanup_owned(owned, preserved)
        details = [f"revert failed: {exc}"]
        if rollback_errors:
            details.append("rollback incomplete: " + "; ".join(rollback_errors))
        if cleanup_errors:
            details.append("cleanup incomplete: " + "; ".join(cleanup_errors))
        if preserved:
            details.append(
                "recovery artifacts: "
                + ", ".join(str(path) for path in sorted(preserved, key=str))
            )
        print("; ".join(details), file=sys.stderr)
        return 1

    cleanup_errors = _cleanup_owned(owned)
    if cleanup_errors:
        print("; ".join(cleanup_errors), file=sys.stderr)
        return 1
    for item in staged:
        cli, scope = item.plan.key
        label = f"{cli}:{scope}" if cli == "claude" else cli
        print(f"[{label}] restored from {item.plan.backup.path}")
    return 0


def _cmd_revert(args: argparse.Namespace) -> int:
    """Restore all selected recovery records as one transaction."""
    try:
        if args.dry_run:
            state = load_state(strict=True)
            state_file = None
        else:
            state, state_file = _strict_state_snapshot()
    except (OSError, RuntimeError, ValueError, SwapStateError) as exc:
        print(exc, file=sys.stderr)
        return 1

    plans, error = _plan_revert(args, state)
    if error or not plans:
        return error
    if args.dry_run:
        for plan in plans:
            cli, scope = plan.key
            label = f"{cli}:{scope}" if cli == "claude" else cli
            print(
                f"[{label}] would restore {plan.config.target} from {plan.backup.path}"
            )
        return 0

    try:
        staged, next_state, state_stage, state_recovery, owned = _stage_revert(
            plans, state, t.cast(RecoveryState, state_file)
        )
    except (OSError, RuntimeError, ValueError) as exc:
        print(f"revert staging failed: {exc}", file=sys.stderr)
        return 1
    return _commit_revert(
        staged,
        next_state,
        t.cast(RecoveryState, state_file),
        state_stage,
        state_recovery,
        owned,
    )


def cmd_revert(args: argparse.Namespace) -> int:
    """Run :func:`_cmd_revert` under the shared mutation lock."""
    if args.dry_run:
        return _cmd_revert(args)
    try:
        with _state_lock():
            return _cmd_revert(args)
    except SwapStateError:
        return 1
    except OSError as exc:
        print(f"swap state unavailable: {exc}", file=sys.stderr)
        return 1


# ---------------------------------------------------------------------------
# doctor — read-only diagnostics
# ---------------------------------------------------------------------------

#: Env vars that, when set, override a CLI's stored subscription/login auth
#: with an API key — a frequent cause of "why is it billing / refusing?"
#: surprises when driving the CLI against a local server. Doctor only reports
#: presence; it never reads the value.
AUTH_ENV_VARS: dict[str, CLIName] = {
    "ANTHROPIC_API_KEY": "claude",
    "OPENAI_API_KEY": "codex",
    "GEMINI_API_KEY": "gemini",
    "GOOGLE_API_KEY": "gemini",
    "XAI_API_KEY": "grok",
    "GROK_API_KEY": "grok",
}


def _env_pair(raw: str) -> tuple[str, str]:
    """Parse a ``KEY=VALUE`` ``--env`` argument, or raise for argparse."""
    key, sep, value = raw.partition("=")
    if not sep or not key:
        msg = f"--env expects KEY=VALUE, got {raw!r}"
        raise argparse.ArgumentTypeError(msg)
    return key, value


def _config_present_clis() -> list[CLIName]:
    """CLIs whose config file exists — enough to *read* entries (no binary needed).

    Distinct from :func:`present_clis`, which also requires the binary on
    ``PATH``. Doctor and the naming hint only inspect config files, so a CLI
    whose binary is absent but whose config is present still has readable
    entries worth surfacing.
    """
    return [cli for cli in ALL_CLIS if CLIS[cli].config_path.exists()]


def _all_server_specs(
    cli: CLIName, config: t.Any, repo: pathlib.Path
) -> dict[str, McpServerSpec]:
    """Enumerate every MCP server entry visible in a CLI's config.

    Spans the scopes a CLI actually keys servers under: Claude's top-level
    user ``mcpServers`` plus this repo's per-project node, and the single
    ``mcpServers`` / ``mcp_servers`` table for the others. Used to detect the
    server-name footgun — the repo registered under a name other than the
    derived default — which a same-name-only lookup misses.
    """
    out: dict[str, McpServerSpec] = {}

    def _add(raw: t.Any) -> None:
        if not isinstance(raw, dict):
            return
        for name, entry in raw.items():
            if not isinstance(entry, dict):
                continue
            out[str(name)] = _spec_from_entry(entry, info=CLIS[cli])

    if cli == "claude":
        _add(_claude_user_servers(config, create=False))
        node = _claude_project_node(config, repo, create=False)
        if node:
            _add(node.get("mcpServers"))
    else:
        _add(_server_map(CLIS[cli], config, create=False))
    return out


def _repo_pointing_names(cli: CLIName, config: t.Any, repo: pathlib.Path) -> list[str]:
    """Server names in this CLI's config whose local checkout is ``repo``."""
    return sorted(
        name
        for name, spec in _all_server_specs(cli, config, repo).items()
        if (local := spec.local_repo_path()) is not None and local == repo
    )


def _naming_hint(repo: pathlib.Path, server: str) -> str | None:
    """Suggest ``--server <name>`` when the repo is registered under another name.

    The derived default (package name minus ``-mcp``) often doesn't match the
    slug the CLIs were actually registered under (e.g. ``tmux`` vs the derived
    ``libtmux``), so a bare run silently operates on a non-existent entry.
    Returns a one-line hint naming the real slug, or ``None`` when the derived
    name is already the registered one (or nothing points here).
    """
    names: set[str] = set()
    server_points = False
    for cli in _config_present_clis():
        try:
            config = load_config(CLIS[cli])
            pointing = _repo_pointing_names(cli, config, repo)
        except (RuntimeError, ValueError, OSError):
            continue
        for name in pointing:
            if name == server:
                server_points = True
            else:
                names.add(name)
    if server_points or not names:
        return None
    pick = min(names)
    return (
        f"note: nothing is registered under server {server!r}, but this repo is "
        f"registered as {sorted(names)} — pass --server {pick} to target it"
    )


def _orphaned_backups(config_path: pathlib.Path) -> list[pathlib.Path]:
    """All ``mcp-swap`` backups sitting next to ``config_path`` (any timestamp)."""
    pattern = config_path.name + BACKUP_SUFFIX_PREFIX + "*"
    return sorted(config_path.parent.glob(pattern))


def cmd_doctor(args: argparse.Namespace) -> int:
    """Report the effective MCP-swap environment without changing anything.

    Read-only. Surfaces the footguns that swap/status don't: the repo
    registered under an unexpected server name, un-reverted swaps and orphaned
    backups accumulating on disk, a state entry whose backup has gone missing
    (so revert would fail), and auth-overriding env vars. It deliberately does
    NOT model each CLI's config-merge behaviour — that is CLI-version-specific
    and lives in documentation, not here.
    """
    repo = pathlib.Path(args.repo).resolve()
    server = args.server or resolve_repo_meta(repo)[0]
    print("mcp-swap doctor")
    print(f"  repo:   {repo}")
    print(f"  server: {server}  (derived default; override with --server)")

    print("  entries by CLI:")
    all_repo_names: set[str] = set()
    for cli in _config_present_clis():
        try:
            config = load_config(CLIS[cli])
            specs = _all_server_specs(cli, config, repo)
            pointing = _repo_pointing_names(cli, config, repo)
        except (RuntimeError, ValueError, OSError) as exc:
            print(f"    [{cli}] config unreadable: {exc}")
            continue
        spec = specs.get(server)
        if spec is not None:
            print(f"    [{cli}] {server} = {_describe_spec(spec, repo)}")
        all_repo_names.update(pointing)
        for name in pointing:
            if name != server:
                print(f"    [{cli}] {name} = local: this repo  (other name)")
    if not all_repo_names:
        print("    (no CLI currently points at this repo)")

    if all_repo_names and server not in all_repo_names:
        pick = min(all_repo_names)
        print(
            f"  ! server name mismatch: this repo is registered as "
            f"{sorted(all_repo_names)}, not {server!r} — use --server {pick}"
        )

    state = load_state()
    if state:
        print("  outstanding swaps (un-reverted):")
        for (cli, scope), entry in sorted(state.items(), key=lambda kv: kv[1].seq_no):
            flag = (
                ""
                if pathlib.Path(entry.backup_path).exists()
                else "  ! BACKUP MISSING — revert would fail for this entry"
            )
            print(f"    {cli}:{scope}  swapped_at={entry.swapped_at}{flag}")

    referenced = {e.backup_path for e in state.values()}
    orphans = [
        b
        for info in CLIS.values()
        for b in _orphaned_backups(info.config_path)
        if str(b) not in referenced
    ]
    if orphans:
        total = sum(b.stat().st_size for b in orphans if b.exists())
        print(
            f"  orphaned backups: {len(orphans)} file(s), {total} bytes not tracked "
            "by state — inspect before deleting: an untracked backup can be the "
            "only surviving pre-swap copy of a config"
        )

    auth_hits = [
        (var, cli) for var, cli in AUTH_ENV_VARS.items() if os.environ.get(var)
    ]
    if auth_hits:
        print("  auth-overriding env vars set:")
        for var, cli in auth_hits:
            print(
                f"    ! {var} overrides {cli}'s stored login — prefix with "
                f"`env -u {var}` to use the subscription/OAuth auth instead"
            )
    return 0


# ---------------------------------------------------------------------------
# argparse glue
# ---------------------------------------------------------------------------


def build_parser() -> argparse.ArgumentParser:
    """Construct the ``argparse`` parser for ``mcp_swap``."""
    p = argparse.ArgumentParser(prog="mcp_swap", description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser(
        "detect", help="list installed CLIs and their config presence"
    ).set_defaults(func=cmd_detect)

    ps = sub.add_parser("status", help="show the current MCP server entry per CLI")
    ps.add_argument("--repo", default=".", help="repo root (default: .)")
    ps.add_argument(
        "--server", help="MCP server name (default: crate name minus '-mcp')"
    )
    ps.add_argument(
        "--cli", action="append", choices=ALL_CLIS, help="limit to one or more CLIs"
    )
    ps.add_argument(
        "--scope",
        choices=ALL_SCOPES,
        default=None,
        help=(
            "Limit Claude output to one scope: 'user' shows only the "
            "top-level mcpServers fallback, 'project' shows only the "
            "projects.<abs>.mcpServers entry. Without this flag, both "
            "Claude scopes print when both have an entry. No-op for "
            "non-Claude CLIs (their config has no per-project layer)."
        ),
    )
    ps.set_defaults(func=cmd_status)

    pu = sub.add_parser(
        "use",
        help="rewrite configs to run a chosen build of this server",
    )
    pu.add_argument("--repo", default=".", help="repo root (default: .)")
    pu.add_argument(
        "--source",
        choices=ALL_SOURCES,
        default="debug",
        help=(
            "Which build to point the agents at. 'debug' and 'release' build "
            "the crate first and name the binary in target/<profile>, so an "
            "agent spawns it directly. 'run' launches through 'cargo run', "
            "rebuilding on every start -- current source with nothing to "
            "remember, at the cost of a build check per launch. 'published' "
            "installs a crates.io release under its own root. 'path' takes a "
            "binary you name with --bin. Default: debug."
        ),
    )
    pu.add_argument(
        "--version",
        help="crates.io version for --source published (e.g. 0.1.0-alpha.3)",
    )
    pu.add_argument("--bin", help="binary to run for --source path")
    pu.add_argument(
        "--crate",
        default=DEFAULT_CRATE,
        help=f"workspace crate providing the server (default: {DEFAULT_CRATE})",
    )
    pu.add_argument(
        "--no-build",
        action="store_true",
        help=(
            "Skip the cargo build that --source debug/release runs first. "
            "Use when the binary is already current and you want the swap "
            "to be instant."
        ),
    )
    pu.add_argument(
        "--no-preflight",
        action="store_true",
        help=(
            "Skip the MCP initialize round trip run before writing. The "
            "probe starts the server once, so a binary that cannot speak "
            "the protocol fails here instead of inside every agent."
        ),
    )
    pu.add_argument(
        "--server", help="MCP server name (default: crate name minus '-mcp')"
    )
    pu.add_argument(
        "--entry", help="binary name (default: the crate's [[bin]] or src/bin stem)"
    )
    pu.add_argument(
        "--env",
        action="append",
        type=_env_pair,
        metavar="KEY=VALUE",
        help=(
            "Extra env var to write into the server entry (repeatable). "
            "Layered on top of any preserved existing env; explicit --env wins. "
            "Use to inject e.g. LIBTMUX_TOOLSETS without a manual post-edit."
        ),
    )
    pu.add_argument("--cli", action="append", choices=ALL_CLIS)
    pu.add_argument(
        "--scope",
        choices=ALL_SCOPES,
        default=None,
        help=(
            "Claude config scope: 'user' rewrites the top-level mcpServers "
            "fallback (every project without an override picks it up), "
            "'project' rewrites projects.<abs>.mcpServers under this repo. "
            "Default 'project'. Silently coerced to 'user' for non-Claude CLIs."
        ),
    )
    pu.add_argument("--dry-run", action="store_true")
    pu.set_defaults(func=cmd_use_local)

    pr = sub.add_parser("revert", help="restore each CLI's config from its swap backup")
    pr.add_argument("--cli", action="append", choices=ALL_CLIS)
    pr.add_argument(
        "--scope",
        choices=ALL_SCOPES,
        default=None,
        help=(
            "Limit revert to one Claude scope. Without this flag, every "
            "recorded scope for the targeted CLIs is reverted."
        ),
    )
    pr.add_argument("--dry-run", action="store_true")
    pr.set_defaults(func=cmd_revert)

    pd = sub.add_parser(
        "doctor", help="report the effective MCP-swap environment (read-only)"
    )
    pd.add_argument("--repo", default=".", help="repo root (default: .)")
    pd.add_argument(
        "--server", help="MCP server name (default: crate name minus '-mcp')"
    )
    pd.set_defaults(func=cmd_doctor)

    return p


def main(argv: list[str] | None = None) -> int:
    """Entry point — dispatches to the selected subcommand."""
    args = build_parser().parse_args(argv)
    return t.cast("int", args.func(args))


if __name__ == "__main__":
    raise SystemExit(main())
