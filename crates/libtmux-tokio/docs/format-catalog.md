# Checked format catalog

The table at the end of this page lists every field in the crate's format
catalog with the owner, context, profiles, first tmux release, decoder, empty
policy and placement the crate gives it. It is not the complete tmux format
table: `format-coverage.txt` records what the crate does about every name tmux
publishes. A test reads the table between the two markers and requires it to
match the catalog in `src/formats.rs` row for row, so a field cannot change in
one and not the other.

Six fields first appear in tmux 3.3 rather than 3.2a: `client_uid`,
`client_user`, `next_session_id`, `pane_start_path`, `uid` and `user`.

The static callbacks and four list-command contexts were audited at every
supported release pin:

| tmux | Commit | Static callbacks | Sessions | Windows | Panes | Clients |
| --- | --- | --- | --- | --- | --- | --- |
| 3.2a | `3b929f3` | [`format.c`](https://github.com/tmux/tmux/blob/3b929f3/format.c) | [source](https://github.com/tmux/tmux/blob/3b929f3/cmd-list-sessions.c) | [source](https://github.com/tmux/tmux/blob/3b929f3/cmd-list-windows.c) | [source](https://github.com/tmux/tmux/blob/3b929f3/cmd-list-panes.c) | [source](https://github.com/tmux/tmux/blob/3b929f3/cmd-list-clients.c) |
| 3.3 | `87fe00e` | [`format.c`](https://github.com/tmux/tmux/blob/87fe00e/format.c) | [source](https://github.com/tmux/tmux/blob/87fe00e/cmd-list-sessions.c) | [source](https://github.com/tmux/tmux/blob/87fe00e/cmd-list-windows.c) | [source](https://github.com/tmux/tmux/blob/87fe00e/cmd-list-panes.c) | [source](https://github.com/tmux/tmux/blob/87fe00e/cmd-list-clients.c) |
| 3.4 | `9ae69c3` | [`format.c`](https://github.com/tmux/tmux/blob/9ae69c3/format.c) | [source](https://github.com/tmux/tmux/blob/9ae69c3/cmd-list-sessions.c) | [source](https://github.com/tmux/tmux/blob/9ae69c3/cmd-list-windows.c) | [source](https://github.com/tmux/tmux/blob/9ae69c3/cmd-list-panes.c) | [source](https://github.com/tmux/tmux/blob/9ae69c3/cmd-list-clients.c) |
| 3.5 | `ac44566` | [`format.c`](https://github.com/tmux/tmux/blob/ac44566/format.c) | [source](https://github.com/tmux/tmux/blob/ac44566/cmd-list-sessions.c) | [source](https://github.com/tmux/tmux/blob/ac44566/cmd-list-windows.c) | [source](https://github.com/tmux/tmux/blob/ac44566/cmd-list-panes.c) | [source](https://github.com/tmux/tmux/blob/ac44566/cmd-list-clients.c) |
| 3.6 | `0dac7fe` | [`format.c`](https://github.com/tmux/tmux/blob/0dac7fe/format.c) | [source](https://github.com/tmux/tmux/blob/0dac7fe/cmd-list-sessions.c) | [source](https://github.com/tmux/tmux/blob/0dac7fe/cmd-list-windows.c) | [source](https://github.com/tmux/tmux/blob/0dac7fe/cmd-list-panes.c) | [source](https://github.com/tmux/tmux/blob/0dac7fe/cmd-list-clients.c) |
| 3.7 | `81f88f8` | [`format.c`](https://github.com/tmux/tmux/blob/81f88f8/format.c) | [source](https://github.com/tmux/tmux/blob/81f88f8/cmd-list-sessions.c) | [source](https://github.com/tmux/tmux/blob/81f88f8/cmd-list-windows.c) | [source](https://github.com/tmux/tmux/blob/81f88f8/cmd-list-panes.c) | [source](https://github.com/tmux/tmux/blob/81f88f8/cmd-list-clients.c) |

Caller-owned registrations were checked at the same pins. The `line`
registration comes from the four list-command sources in each audit row. The
three command-list fields come from `cmd-list-keys.c` in
[3.2a](https://github.com/tmux/tmux/blob/3b929f3/cmd-list-keys.c),
[3.3](https://github.com/tmux/tmux/blob/87fe00e/cmd-list-keys.c),
[3.4](https://github.com/tmux/tmux/blob/9ae69c3/cmd-list-keys.c),
[3.5](https://github.com/tmux/tmux/blob/ac44566/cmd-list-keys.c), and
[3.6](https://github.com/tmux/tmux/blob/0dac7fe/cmd-list-keys.c), then
`cmd-list-commands.c` in
[3.7](https://github.com/tmux/tmux/blob/81f88f8/cmd-list-commands.c).
`current_file` comes from `cfg.c` in
[3.2a](https://github.com/tmux/tmux/blob/3b929f3/cfg.c),
[3.3](https://github.com/tmux/tmux/blob/87fe00e/cfg.c),
[3.4](https://github.com/tmux/tmux/blob/9ae69c3/cfg.c),
[3.5](https://github.com/tmux/tmux/blob/ac44566/cfg.c),
[3.6](https://github.com/tmux/tmux/blob/0dac7fe/cfg.c), and
[3.7](https://github.com/tmux/tmux/blob/81f88f8/cfg.c). The copy-mode fields
come from `window-copy.c` in
[3.2a](https://github.com/tmux/tmux/blob/3b929f3/window-copy.c),
[3.3](https://github.com/tmux/tmux/blob/87fe00e/window-copy.c),
[3.4](https://github.com/tmux/tmux/blob/9ae69c3/window-copy.c),
[3.5](https://github.com/tmux/tmux/blob/ac44566/window-copy.c),
[3.6](https://github.com/tmux/tmux/blob/0dac7fe/window-copy.c), and
[3.7](https://github.com/tmux/tmux/blob/81f88f8/window-copy.c).

At all six pins, the list calls pass these unchanged `format_defaults`
contexts: Sessions `(NULL, s, NULL, NULL)`, Windows
`(NULL, s, wl, NULL)`, Panes `(NULL, s, wl, wp)`, and Clients
`(c, NULL, NULL, NULL)`. `format_defaults` fixes the original row type
before deriving missing Session, winlink, or Pane context. Every audited
`list-clients` skips an unattached Client before that call, so all emitted
Client rows have sound derived Session, Window, and Pane contexts. Technical
profile admission is therefore broader than placement in an intrinsic
`*Info` value. In particular, `client_mode_format` ignores its context and
returns the global `window_client_mode.default_format`.

<!-- BEGIN CHECKED FORMAT CATALOG -->
| field | owner | context | profiles | minimum | decoder | empty | placement |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `active_window_index` | session | session | all | 3.2a | u32 | required | catalog-only |
| `alternate_saved_x` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `alternate_saved_y` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `bracket_paste_flag` | pane | pane | all | 3.7 | bool | required | pane-info |
| `buffer_name` | buffer | buffer | none | 3.2a | text | required | catalog-only |
| `buffer_sample` | buffer | buffer | none | 3.2a | text | available | catalog-only |
| `buffer_size` | buffer | buffer | none | 3.2a | u64 | required | catalog-only |
| `client_activity` | client | client | clients | 3.2a | timestamp | required | client-info |
| `client_cell_height` | client | client | clients | 3.2a | u32 | absent | client-info |
| `client_cell_width` | client | client | clients | 3.2a | u32 | absent | client-info |
| `client_control_mode` | client | client | clients | 3.2a | bool | required | client-info |
| `client_created` | client | client | clients | 3.2a | timestamp | required | client-info |
| `client_discarded` | client | client | clients | 3.2a | u64 | required | client-info |
| `client_flags` | client | client | clients | 3.2a | text | available | client-info |
| `client_height` | client | client | clients | 3.2a | u32 | absent | client-info |
| `client_key_table` | client | client | clients | 3.2a | text | required | client-info |
| `client_last_session` | client-attachment | client | clients | 3.2a | text | absent | catalog-only |
| `client_mode_format` | mode | none | all | 3.2a | text | required | catalog-only |
| `client_name` | client | client | clients | 3.2a | text | required | client-info |
| `client_pid` | client | client | clients | 3.2a | u32 | required | client-info |
| `client_prefix` | client | client | clients | 3.2a | bool | required | client-info |
| `client_readonly` | client | client | clients | 3.2a | bool | required | client-info |
| `client_session` | client-attachment | client | clients | 3.2a | text | absent | catalog-only |
| `client_termfeatures` | client | client | clients | 3.2a | text | available | client-info |
| `client_termname` | client | client | clients | 3.2a | text | available | client-info |
| `client_termtype` | client | client | clients | 3.2a | text | available | client-info |
| `client_tty` | client | client | clients | 3.2a | text | available | client-info |
| `client_uid` | client | client | clients | 3.3 | u32 | absent | client-info |
| `client_user` | client | client | clients | 3.3 | text | absent | client-info |
| `client_utf8` | client | client | clients | 3.2a | bool | required | client-info |
| `client_width` | client | client | clients | 3.2a | u32 | required | client-info |
| `client_written` | client | client | clients | 3.2a | u64 | required | client-info |
| `command_list_alias` | command | command | none | 3.2a | text | available | catalog-only |
| `command_list_name` | command | command | none | 3.2a | text | required | catalog-only |
| `command_list_usage` | command | command | none | 3.2a | text | available | catalog-only |
| `config_files` | server | none | all | 3.2a | text | available | catalog-only |
| `copy_cursor_line` | copy-mode | copy-mode | none | 3.2a | text | available | catalog-only |
| `copy_cursor_word` | copy-mode | copy-mode | none | 3.2a | text | available | catalog-only |
| `copy_cursor_x` | copy-mode | copy-mode | none | 3.2a | i32 | required | catalog-only |
| `copy_cursor_y` | copy-mode | copy-mode | none | 3.2a | i32 | required | catalog-only |
| `current_file` | config | config | none | 3.2a | text | required | catalog-only |
| `cursor_character` | pane | pane | all | 3.2a | text | absent | pane-info |
| `cursor_flag` | pane | pane | all | 3.2a | bool | required | pane-info |
| `cursor_x` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `cursor_y` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `history_bytes` | pane | pane | all | 3.2a | u64 | required | pane-info |
| `history_limit` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `history_size` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `insert_flag` | pane | pane | all | 3.2a | bool | required | pane-info |
| `keypad_cursor_flag` | pane | pane | all | 3.2a | bool | required | pane-info |
| `keypad_flag` | pane | pane | all | 3.2a | bool | required | pane-info |
| `last_window_index` | session | session | all | 3.2a | u32 | required | catalog-only |
| `line` | list-row | list-row | all | 3.2a | u32 | required | catalog-only |
| `mouse_all_flag` | pane | pane | all | 3.2a | bool | required | pane-info |
| `mouse_any_flag` | pane | pane | all | 3.2a | bool | required | pane-info |
| `mouse_button_flag` | pane | pane | all | 3.2a | bool | required | pane-info |
| `mouse_sgr_flag` | pane | pane | all | 3.2a | bool | required | pane-info |
| `mouse_standard_flag` | pane | pane | all | 3.2a | bool | required | pane-info |
| `next_session_id` | server | none | all | 3.3 | session-id | required | catalog-only |
| `origin_flag` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_active` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_at_bottom` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_at_left` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_at_right` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_at_top` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_bg` | pane | pane | all | 3.2a | text | required | pane-info |
| `pane_bottom` | pane | pane | all | 3.2a | i32 | required | pane-info |
| `pane_current_command` | pane | pane | all | 3.2a | text | absent | pane-info |
| `pane_current_path` | pane | pane | all | 3.2a | text | absent | pane-info |
| `pane_dead` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_dead_signal` | pane | pane | all | 3.3 | text | absent | pane-info |
| `pane_dead_status` | pane | pane | all | 3.2a | u8 | absent | pane-info |
| `pane_dead_time` | pane | pane | all | 3.3 | timestamp | absent | pane-info |
| `pane_fg` | pane | pane | all | 3.2a | text | required | pane-info |
| `pane_flags` | pane | pane | all | 3.7 | text | available | pane-info |
| `pane_floating_flag` | pane | pane | all | 3.7 | bool | required | pane-info |
| `pane_format` | list-row | format-type | all | 3.2a | bool | required | catalog-only |
| `pane_height` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `pane_id` | pane | pane | all | 3.2a | pane-id | required | pane-info |
| `pane_in_mode` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `pane_index` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `pane_input_off` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_last` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_left` | pane | pane | all | 3.2a | i32 | required | pane-info |
| `pane_marked` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_marked_set` | server | pane | all | 3.2a | bool | required | catalog-only |
| `pane_mode` | pane | pane | all | 3.2a | text | absent | pane-info |
| `pane_path` | pane | pane | all | 3.2a | text | available | pane-info |
| `pane_pb_progress` | pane | pane | all | 3.7 | pane-progress | required | pane-info |
| `pane_pb_state` | pane | pane | all | 3.7 | pane-progress-state | required | pane-info |
| `pane_pid` | pane | pane | all | 3.2a | u32 | absent | pane-info |
| `pane_pipe` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_pipe_pid` | pane | pane | all | 3.7 | u32 | absent | pane-info |
| `pane_right` | pane | pane | all | 3.2a | i32 | required | pane-info |
| `pane_search_string` | pane | pane | all | 3.2a | text | available | pane-info |
| `pane_start_command` | pane | pane | all | 3.2a | text | available | pane-info |
| `pane_start_path` | pane | pane | all | 3.3 | text | available | pane-info |
| `pane_synchronized` | pane | pane | all | 3.2a | bool | required | pane-info |
| `pane_tabs` | pane | pane | all | 3.2a | text | available | pane-info |
| `pane_title` | pane | pane | all | 3.2a | text | available | pane-info |
| `pane_top` | pane | pane | all | 3.2a | i32 | required | pane-info |
| `pane_tty` | pane | pane | all | 3.2a | text | required | pane-info |
| `pane_unseen_changes` | pane | pane | all | 3.4 | bool | required | pane-info |
| `pane_width` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `pane_x` | pane | pane | all | 3.7 | i32 | required | pane-info |
| `pane_y` | pane | pane | all | 3.7 | i32 | required | pane-info |
| `pane_z` | pane | pane | all | 3.7 | u32 | required | pane-info |
| `pane_zoomed_flag` | pane | pane | all | 3.7 | bool | required | pane-info |
| `pid` | server | none | all | 3.2a | u32 | required | catalog-only |
| `scroll_position` | copy-mode | pane | all | 3.2a | i32 | absent | pane-info |
| `scroll_region_lower` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `scroll_region_upper` | pane | pane | all | 3.2a | u32 | required | pane-info |
| `search_match` | copy-mode | copy-mode | none | 3.2a | text | absent | catalog-only |
| `selection_end_x` | copy-mode | copy-mode | none | 3.2a | i32 | absent | catalog-only |
| `selection_end_y` | copy-mode | copy-mode | none | 3.2a | i32 | absent | catalog-only |
| `selection_start_x` | copy-mode | copy-mode | none | 3.2a | i32 | absent | catalog-only |
| `selection_start_y` | copy-mode | copy-mode | none | 3.2a | i32 | absent | catalog-only |
| `session_activity` | session | session | all | 3.2a | timestamp | required | session-info |
| `session_alerts` | session | session | all | 3.2a | text | available | catalog-only |
| `session_attached` | session | session | all | 3.2a | u32 | required | session-info |
| `session_attached_list` | session | session | all | 3.2a | text | available | catalog-only |
| `session_created` | session | session | all | 3.2a | timestamp | required | session-info |
| `session_format` | list-row | format-type | all | 3.2a | bool | required | catalog-only |
| `session_group` | session | session | all | 3.2a | text | absent | catalog-only |
| `session_group_attached` | session | session | all | 3.2a | u32 | absent | catalog-only |
| `session_group_attached_list` | session | session | all | 3.2a | text | absent | catalog-only |
| `session_group_list` | session | session | all | 3.2a | text | absent | catalog-only |
| `session_group_many_attached` | session | session | all | 3.2a | bool | absent | catalog-only |
| `session_group_size` | session | session | all | 3.2a | u32 | absent | catalog-only |
| `session_grouped` | session | session | all | 3.2a | bool | required | catalog-only |
| `session_id` | session | session | all | 3.2a | session-id | required | session-info |
| `session_last_attached` | session | session | all | 3.2a | timestamp | absent | session-info |
| `session_many_attached` | session | session | all | 3.2a | bool | required | session-info |
| `session_marked` | session | session | all | 3.2a | bool | required | catalog-only |
| `session_name` | session | session | all | 3.2a | text | available | session-info |
| `session_path` | session | session | all | 3.2a | text | available | session-info |
| `session_stack` | session | session | all | 3.2a | text | required | catalog-only |
| `session_windows` | session | session | all | 3.2a | u32 | required | session-info |
| `socket_path` | server | none | all | 3.2a | text | required | catalog-only |
| `start_time` | server | none | all | 3.2a | timestamp | required | catalog-only |
| `synchronized_output_flag` | pane | pane | all | 3.7 | bool | required | pane-info |
| `uid` | server | none | all | 3.3 | u32 | required | catalog-only |
| `user` | server | none | all | 3.3 | text | absent | catalog-only |
| `version` | server | none | all | 3.2a | text | required | catalog-only |
| `window_active` | window-link | window-link | all | 3.2a | bool | required | catalog-only |
| `window_active_clients` | window | window-link | all | 3.2a | u32 | required | catalog-only |
| `window_active_clients_list` | window | window-link | all | 3.2a | text | available | catalog-only |
| `window_active_sessions` | window | window-link | all | 3.2a | u32 | required | catalog-only |
| `window_active_sessions_list` | window | window-link | all | 3.2a | text | available | catalog-only |
| `window_activity` | window | window | all | 3.2a | timestamp | required | window-info |
| `window_activity_flag` | window-link | window-link | all | 3.2a | bool | required | catalog-only |
| `window_bell_flag` | window-link | window-link | all | 3.2a | bool | required | catalog-only |
| `window_bigger` | client-window-view | client | clients | 3.2a | bool | required | catalog-only |
| `window_cell_height` | window | window | all | 3.2a | u32 | required | window-info |
| `window_cell_width` | window | window | all | 3.2a | u32 | required | window-info |
| `window_end_flag` | window-link | window-link | all | 3.2a | bool | required | catalog-only |
| `window_flags` | window-link | window-link | all | 3.2a | text | available | catalog-only |
| `window_format` | list-row | format-type | all | 3.2a | bool | required | catalog-only |
| `window_height` | window | window | all | 3.2a | u32 | required | window-info |
| `window_id` | window | window | all | 3.2a | window-id | required | window-info |
| `window_index` | window-link | window-link | all | 3.2a | i32 | required | catalog-only |
| `window_last_flag` | window-link | window-link | all | 3.2a | bool | required | catalog-only |
| `window_layout` | window | window | all | 3.2a | text | required | window-info |
| `window_linked` | window-link | window-link | all | 3.2a | bool | required | catalog-only |
| `window_linked_sessions` | window | window-link | all | 3.2a | u32 | required | catalog-only |
| `window_linked_sessions_list` | window | window-link | all | 3.2a | text | available | catalog-only |
| `window_marked_flag` | window-link | window-link | all | 3.2a | bool | required | catalog-only |
| `window_name` | window | window | all | 3.2a | text | available | window-info |
| `window_offset_x` | client-window-view | client | clients | 3.2a | u32 | absent | catalog-only |
| `window_offset_y` | client-window-view | client | clients | 3.2a | u32 | absent | catalog-only |
| `window_panes` | window | window | all | 3.2a | u32 | required | window-info |
| `window_raw_flags` | window-link | window-link | all | 3.2a | text | available | catalog-only |
| `window_silence_flag` | window-link | window-link | all | 3.2a | bool | required | catalog-only |
| `window_stack_index` | window-link | window-link | all | 3.2a | u32 | required | catalog-only |
| `window_start_flag` | window-link | window-link | all | 3.2a | bool | required | catalog-only |
| `window_visible_layout` | window | window | all | 3.2a | text | required | window-info |
| `window_width` | window | window | all | 3.2a | u32 | required | window-info |
| `window_zoomed_flag` | window | window | all | 3.2a | bool | required | window-info |
| `wrap_flag` | pane | pane | all | 3.2a | bool | required | pane-info |
<!-- END CHECKED FORMAT CATALOG -->
