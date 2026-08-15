# tmux-workspace

Build tmux workspaces from [tmuxp](https://tmuxp.git-pull.com/)-style YAML,
using [libtmux](https://docs.rs/libtmux).

```yaml
session_name: dev
windows:
  - window_name: editor
    panes:
      - vim
      - htop
```

```rust,no_run
use tmux_workspace::{Workspace, WorkspaceBuilder};

# async fn build() -> Result<(), tmux_workspace::BuildError> {
let workspace = Workspace::from_yaml(std::fs::read_to_string("dev.yaml")?.as_str())?;
let server = libtmux::Server::new()?;
let session = WorkspaceBuilder::new(&server).build(&workspace).await?;
# Ok(())
# }
```

## Not published

This crate is `publish = false`, and that is the point of it rather than an
oversight.

It exists to drive the `libtmux` public API from outside the crate that
defines it. Tests written inside `libtmux` can reach `pub(crate)` items and
can be written around whatever shape the internals happen to have; a separate
crate cannot. So this one builds something real — sessions, windows, panes,
splits, and the layout a workspace file describes — using only what a
published consumer can see. An API that is awkward to use from here is
awkward for everyone, and that shows up as a compile error rather than as a
review comment.

That job does not need a crates.io listing, so it does not have one. The
publishing metadata the sibling crates carry — keywords, categories, an
`include` list, a docs.rs section — is deliberately absent, because none of it
would mean anything for a crate that is never uploaded.

## Development

```console
$ cargo test -p tmux-workspace
```

The tests drive a real tmux on an isolated socket, so tmux must be on `$PATH`.

## License

MIT
