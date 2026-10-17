//! Which tmux versions libtmux supports.
//!
//! "Support" here means tmux version support, not helper code. [`versions`]
//! checks that a daemon is a supported release before any command runs.

pub(crate) mod versions;
