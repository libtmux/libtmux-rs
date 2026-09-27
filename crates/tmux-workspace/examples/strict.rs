//! What the reader does with a key it does not act on.
//!
//! [`Workspace::from_yaml`] loads a richer tmuxp file and records what it left
//! out, so nothing is lost silently: the key is in `unsupported_keys`.
//! [`Workspace::from_yaml_strict`] refuses it instead, the policy the
//! `tmux-workspace` command applies, so a typo fails where it was made. A key
//! starting with `x-` is inert either way. No tmux is needed.
//!
//! ```console
//! $ cargo run --example strict
//! ```

use tmux_workspace::Workspace;

const DOCUMENT: &str = "
session_name: api
x-team: backend
windows:
  - window_name: editor
    panes:
      - vim
    focuss: true
";

fn main() {
    match Workspace::from_yaml(DOCUMENT) {
        Ok(workspace) => {
            let window = &workspace.windows[0];
            println!(
                "lenient: loaded; the window left out {:?}",
                window.unsupported_keys
            );
        }
        Err(error) => println!("lenient: refused: {error}"),
    }
    match Workspace::from_yaml_strict(DOCUMENT) {
        Ok(_) => println!("strict: loaded, which a typo should not"),
        Err(error) => println!("strict: refused: {error}"),
    }
}
