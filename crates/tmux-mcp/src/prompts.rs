use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{PromptMessage, Role};
use rmcp::{prompt, prompt_router, schemars};
use serde::{Deserialize, Serialize};

use crate::TmuxTools;

/// Arguments naming one pane, for a prompt.
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct PanePrompt {
    /// The `%`-prefixed pane id.
    pub pane: String,
}

/// Arguments for the run-and-wait recipe.
#[derive(Debug, Deserialize, Serialize, schemars::JsonSchema)]
pub struct RunPrompt {
    /// The `%`-prefixed pane id to run in.
    pub pane: String,
    /// The shell command to run.
    pub command: String,
}

/// Recipes for the tool combinations that are easy to get wrong.
///
/// Three, and no more without a reason. A prompt earns its place by teaching
/// a *composition* — something no single tool's description can say, because
/// it is about which tool to reach for and in what order. Anything that fits
/// in one tool's description belongs there instead, where an agent meets it
/// while choosing.
#[allow(
    missing_docs,
    reason = "the prompt macro generates a metadata function without a doc attribute, \
              unlike the tool macro, which does; the prompts themselves are documented"
)]
#[prompt_router]
impl TmuxTools {
    /// Run a command and act on its exit status.
    #[prompt(
        name = "run_and_wait",
        title = "Run A Command And Wait",
        description = "Run a shell command in a pane and act on how it finished, rather \
                       than typing it and reading the screen afterwards."
    )]
    pub async fn run_and_wait(
        &self,
        Parameters(RunPrompt { pane, command }): Parameters<RunPrompt>,
    ) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "In tmux pane {pane}, run:\n\n    {command}\n\n\
                 Use run_command, not send_keys. It waits for the command to finish and \
                 comes back with an exit_status and the output the command itself wrote — \
                 no prompt, no echo, nothing that scrolled past. Decide from exit_status; \
                 read output only to explain it.\n\n\
                 Two answers are not failures and should not be retried blindly. \
                 outcome=deadline means the time ran out and the command is still running, \
                 so the pane is busy: continue from the returned job id with job_status or \
                 forget its retained output with forget_job. outcome=no_shell means the pane was not at a \
                 prompt, so the text went into whatever is running there; inspect the same \
                 job and look with snapshot_pane before trying again."
            ),
        )]
    }

    /// Get a wedged pane back to a prompt.
    #[prompt(
        name = "interrupt_gracefully",
        title = "Interrupt A Busy Pane",
        description = "Stop whatever a pane is running and get it back to a shell prompt, \
                       without killing the pane."
    )]
    pub async fn interrupt_gracefully(
        &self,
        Parameters(PanePrompt { pane }): Parameters<PanePrompt>,
    ) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Get tmux pane {pane} back to a shell prompt without destroying it.\n\n\
                 Look first with snapshot_pane: it reports the cursor and whether the pane \
                 is in copy mode. A pane in copy mode is not busy at all — it is just not \
                 listening, and q leaves it.\n\n\
                 To stop a running command, send the key rather than the letters: \
                 send_keys with keys=[\"C-c\"]. Passing \"C-c\" as text types three \
                 characters at the command instead of interrupting it.\n\n\
                 Give it a moment and check again — a shell takes a beat to reclaim the \
                 terminal. If C-c does not take, C-\\\\ is stronger. Reach for kill_pane \
                 only when the pane itself is beyond saving, and remember that it destroys \
                 whatever was in it."
            ),
        )]
    }

    /// Work out what a pane is doing.
    #[prompt(
        name = "diagnose_pane",
        title = "Diagnose A Pane",
        description = "Work out what a pane is doing and why, using the tools in the order \
                       that answers it in fewest calls."
    )]
    pub async fn diagnose_pane(
        &self,
        Parameters(PanePrompt { pane }): Parameters<PanePrompt>,
    ) -> Vec<PromptMessage> {
        vec![PromptMessage::new_text(
            Role::User,
            format!(
                "Work out what tmux pane {pane} is doing and report it.\n\n\
                 Start with snapshot_pane: one call gives what the pane shows, what is \
                 running in it, the cursor, and whether it is in copy mode or dead. A \
                 cursor at the start of a fresh line usually means a shell waiting; a \
                 pane whose command is not a shell is busy.\n\n\
                 If the visible screen is not enough, capture_pane with history=true \
                 reaches what has scrolled away. To follow it as it goes, take a cursor \
                 from capture_since and call again later with it — that returns only what \
                 is new, rather than the whole screen each time.\n\n\
                 If you are not sure this is even the right pane, search_panes finds which \
                 pane is showing a piece of text. The listing tools will not: they read \
                 names and commands, not what a terminal displays."
            ),
        )]
    }
}

pub(super) fn router() -> rmcp::handler::server::router::prompt::PromptRouter<TmuxTools> {
    TmuxTools::prompt_router()
}
