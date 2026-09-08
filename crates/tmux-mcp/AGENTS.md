# AGENTS.md

Rules for `crates/tmux-mcp/`. Repository-wide rules remain in
[`AGENTS.md`](../../AGENTS.md) and the writing and contribution guides it
names.

## MCP surface boundary

The MCP is a curated semantic surface for detached-safe operations. Library
parity does not imply MCP parity. Keep modal human-client interfaces out when a
noninteractive operation serves the agent: copy mode, clock mode, choose-tree,
prompts, menus, popups, and mouse gestures belong to the attached client.

Read terminal text through capture, history, snapshot, search, and
`capture_since`; report a pane's mode rather than entering or cancelling it.
Paired enter/exit cleanup, unclear ownership, and dependence on key tables,
mouse events, clipboards, or timing are signals that an operation does not
belong in MCP. Retain the core `libtmux` API even when MCP omits the command.

Every public tool belongs to exactly one ADR toolset. The native manifest owns
runtime registration, schemas, generated documentation, and manifest tests;
do not maintain an independent public inventory.
