# Moving from an earlier alpha

Retired names have no hidden aliases. No prompts are registered; their recipes
now run in the client. Update existing client calls with this mapping. The
[README](../README.md) shows the current tools, and the
[contracts](contracts.md) say what each selection setting does.

| Earlier surface | Current path |
|---|---|
| `--safety`, `LIBTMUX_SAFETY`, `TMUX_MCP_SAFETY` | Select unordered `LIBTMUX_TOOLSETS`, then exact inclusions or exclusions. |
| `--confirm`, `--no-confirm`, `TMUX_MCP_CONFIRM` | Remove them. The server has no confirmation policy; clients decide approval from each tool's MCP annotations. |
| `list_session_windows`, `list_window_panes` | Use [`list_windows`][list_windows] or [`list_panes`][list_panes], then filter the returned stable `session_id` or `window_id`. |
| `describe` | Use [`get_server_info`][get_server_info], [`get_session_info`][get_session_info], [`get_window_info`][get_window_info], or [`get_pane_info`][get_pane_info]. |
| `list_servers` or per-call socket selection | Run one MCP process per socket and use [`get_server_info`][get_server_info] for its pinned server. |
| `expand_format` | Use [`get_tmux_variables`][get_tmux_variables] for validated variable names; arbitrary tmux-format evaluation has no public replacement. |
| `what_changed`, `watch_pane` | Follow a known pane with [`capture_since`][capture_since] or wait with [`wait_for_text`][wait_for_text]; there is no global activity feed or subscription. |
| `find_panes`, `find_sessions` | Use [`search_panes`][search_panes] for terminal content, [`find_pane_by_position`][find_pane_by_position] for layout, or filter [`list_panes`][list_panes] and [`list_sessions`][list_sessions] locally. |
| `run_command` | Use [`run_shell_command`][run_shell_command]. |
| `wait_for_idle` | Use a concrete [`wait_for_text`][wait_for_text] condition or inspect new output with [`capture_since`][capture_since]; no idle heuristic remains. |
| `start_command`, `job_status`, `list_jobs`, `forget_job` | No background handle remains. Use bounded [`run_shell_command`][run_shell_command], or drive a pane and observe it with [`capture_since`][capture_since]. |
| `new_window`, `split_pane` | Use [`create_window`][create_window] and [`split_window`][split_window]. |
| `rename` | Use [`rename_session`][rename_session], [`rename_window`][rename_window], or [`set_pane_title`][set_pane_title]. |
| `pipe_pane` | No shell-command pipe route remains; read with [`capture_pane`][capture_pane] or [`capture_since`][capture_since]. |
| `clear_pane` | Use teardown tool [`clear_pane_scrollback`][clear_pane_scrollback]. |
| `set_option` | Use constrained tools such as [`set_mouse_enabled`][set_mouse_enabled], [`set_history_limit`][set_history_limit], or [`set_synchronize_panes`][set_synchronize_panes]. |
| `set_environment` | No generic caller-controlled environment route remains. |
| `show_environment` values | Values are withheld by default; allow names with [`LIBTMUX_ENVIRONMENT_VALUES`]. |
| `run_plan` | Use [`call_read_tools_batch`][call_read_tools_batch] for inspect-only batches; issue typed state-changing calls separately. |
| `kill_server` | Kill selected sessions explicitly or administer the server outside MCP. |
| `tmux://server` | Use [`get_server_info`][get_server_info]; `tmux://capabilities` adds the frozen connection and selection boundary. |
| `tmux://sessions`, `tmux://windows`, `tmux://panes` | Use [`list_sessions`][list_sessions], [`list_windows`][list_windows], and [`list_panes`][list_panes]. |
| `tmux://sessions/{name}`, `tmux://panes/{id}` | Use [`get_session_info`][get_session_info] and [`get_pane_info`][get_pane_info]. |
| `tmux://sessions/{name}/windows`, `tmux://sessions/{name}/windows/{index}` | Use [`list_windows`][list_windows], filter by `session_id`, then pass the returned stable id to [`get_window_info`][get_window_info]. |
| `tmux://panes/{id}/content` | Use [`capture_pane`][capture_pane] or continue from a cursor with [`capture_since`][capture_since]. |
| `enter_copy_mode`, `exit_copy_mode` | Read with capture, snapshot, search, or cursor tools. The attached person owns pane modes. |
| Prompt `run_and_wait` | Use one [`run_shell_command`][run_shell_command]; decide from `outcome` and `exit_status`. A deadline stops the wait, not the pane command. |
| Prompt `interrupt_gracefully` | Start with [`snapshot_pane`][snapshot_pane]. If `in_mode`, keep observing and let the attached person leave the mode. Otherwise use [`send_keys`][send_keys] with `keys: ["C-c"]`, not `text: "C-c"`, then observe with [`capture_since`][capture_since] or [`wait_for_text`][wait_for_text]. `keys: ["C-\\"]` is stronger. Either, sent alone, reaches a command an active [`run_shell_command`][run_shell_command] still reserves; [`respawn_pane`][respawn_pane] with `kill_first: true` replaces a program that ignores both. Do not turn pane recovery into teardown. |
| Prompt `diagnose_pane` | Start with [`snapshot_pane`][snapshot_pane]; inspect `pane.command`, `dead`, `in_mode`, `mode`, `dropped`, and `content`. Repeat with `history: true` when the visible screen is insufficient, follow later output with [`capture_since`][capture_since], and use [`search_panes`][search_panes] if the target is uncertain. |

[call_read_tools_batch]: https://libtmux.org/en/rs/latest/mcp/tools/call_read_tools_batch/
[capture_pane]: https://libtmux.org/en/rs/latest/mcp/tools/capture_pane/
[capture_since]: https://libtmux.org/en/rs/latest/mcp/tools/capture_since/
[clear_pane_scrollback]: https://libtmux.org/en/rs/latest/mcp/tools/clear_pane_scrollback/
[create_window]: https://libtmux.org/en/rs/latest/mcp/tools/create_window/
[find_pane_by_position]: https://libtmux.org/en/rs/latest/mcp/tools/find_pane_by_position/
[get_pane_info]: https://libtmux.org/en/rs/latest/mcp/tools/get_pane_info/
[get_server_info]: https://libtmux.org/en/rs/latest/mcp/tools/get_server_info/
[get_session_info]: https://libtmux.org/en/rs/latest/mcp/tools/get_session_info/
[get_tmux_variables]: https://libtmux.org/en/rs/latest/mcp/tools/get_tmux_variables/
[get_window_info]: https://libtmux.org/en/rs/latest/mcp/tools/get_window_info/
[list_panes]: https://libtmux.org/en/rs/latest/mcp/tools/list_panes/
[list_sessions]: https://libtmux.org/en/rs/latest/mcp/tools/list_sessions/
[list_windows]: https://libtmux.org/en/rs/latest/mcp/tools/list_windows/
[rename_session]: https://libtmux.org/en/rs/latest/mcp/tools/rename_session/
[rename_window]: https://libtmux.org/en/rs/latest/mcp/tools/rename_window/
[respawn_pane]: https://libtmux.org/en/rs/latest/mcp/tools/respawn_pane/
[run_shell_command]: https://libtmux.org/en/rs/latest/mcp/tools/run_shell_command/
[search_panes]: https://libtmux.org/en/rs/latest/mcp/tools/search_panes/
[send_keys]: https://libtmux.org/en/rs/latest/mcp/tools/send_keys/
[set_history_limit]: https://libtmux.org/en/rs/latest/mcp/tools/set_history_limit/
[set_mouse_enabled]: https://libtmux.org/en/rs/latest/mcp/tools/set_mouse_enabled/
[set_pane_title]: https://libtmux.org/en/rs/latest/mcp/tools/set_pane_title/
[set_synchronize_panes]: https://libtmux.org/en/rs/latest/mcp/tools/set_synchronize_panes/
[snapshot_pane]: https://libtmux.org/en/rs/latest/mcp/tools/snapshot_pane/
[split_window]: https://libtmux.org/en/rs/latest/mcp/tools/split_window/
[wait_for_text]: https://libtmux.org/en/rs/latest/mcp/tools/wait_for_text/
[`LIBTMUX_ENVIRONMENT_VALUES`]: https://libtmux.org/en/rs/latest/mcp/reference/mcp-policy-environment_values_env/
