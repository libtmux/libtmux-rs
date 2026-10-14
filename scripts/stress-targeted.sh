#!/usr/bin/env bash
# The test binaries a flake fix touched and the ones that share their shape:
# waits on tmux, process cleanup, pause timing. A shorter set than the full
# suite, so it can repeat on the shared macOS runners.
set -u
status=0
run() { cargo test --locked --all-features --no-fail-fast "$@" || status=1; }

run -p libtmux --test limits --test commands --test options --test mutations \
  --test filter_hierarchy --test plan --test control --test server_command \
  --test test_server
run -p libtmux --lib -- control::lifecycle_tests internal::subprocess
run -p tmux-workspace --test build --test cli_signals --test cli
run -p mcp-swap --test preflight
run -p tmux-mcp --test echo_contract --test agent --test paste_enter
exit "$status"
