use std::ffi::OsString;

use super::{Chooser, Server};
use crate::client::Client;
use crate::internal::listing;
use crate::{Command, Error};

impl Server {
    /// Show a popup over a client, running a command inside it.
    ///
    /// This needs a client with a terminal, so it fails on a server nothing is
    /// attached to.
    ///
    /// Unlike [`Self::command_prompt`] and [`Self::display_menu`], this does
    /// not wait for a person -- but it does wait for the command. The popup is
    /// opened with `-E`, so it closes when what runs inside it exits, and this
    /// returns then. A command that does not end holds the call until the
    /// dispatch timeout does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when no suitable client exists, when tmux refuses
    /// the command, and when the dispatch timeout expires before it exits.
    pub async fn display_popup(
        &self,
        client: Option<&Client>,
        command: impl Into<OsString>,
    ) -> Result<(), Error> {
        let mut popup = Command::new("display-popup").arg("-E");
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), "display-popup")?;
            popup = popup
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }

        listing::mutate(&self.core, "display-popup", popup.arg(command.into())).await
    }

    /// Show a menu over a client.
    ///
    /// Items are `(label, key, command)` triples in the order tmux should show
    /// them. This needs a client with a terminal.
    ///
    /// Like [`Self::command_prompt`], this waits for the person: tmux holds the
    /// invocation until an item is chosen or the menu is dismissed, and the
    /// dispatch timeout is what ends the wait when nobody does.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when no suitable client exists, when tmux refuses
    /// an item, and when the dispatch timeout expires before anyone chooses.
    pub async fn display_menu(
        &self,
        client: Option<&Client>,
        title: &str,
        items: impl IntoIterator<Item = (String, String, String)>,
    ) -> Result<(), Error> {
        let mut menu = Command::new("display-menu")
            .arg("-T")
            .arg(OsString::from(title));
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), "display-menu")?;
            menu = menu
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }
        for (label, key, command) in items {
            menu = menu
                .arg(OsString::from(label))
                .arg(OsString::from(key))
                .arg(OsString::from(command));
        }

        listing::mutate(&self.core, "display-menu", menu).await
    }

    /// Open a command prompt on a client.
    ///
    /// The prompt runs `command` once the user answers, with `%%` replaced by
    /// what they typed.
    ///
    /// This does not return when the prompt opens. tmux holds the invocation
    /// until somebody answers or dismisses it, so a caller is waiting on a
    /// person -- and on a server nobody is watching, on nobody. The dispatch
    /// timeout is what ends that wait: with the default it fails after thirty
    /// seconds having opened a prompt that is still there. Give the call a
    /// server whose `default_timeout` suits a human, or drive it from a task
    /// that may take that long.
    ///
    /// Passing a client is not what decides this. Both forms wait; naming one
    /// only decides which terminal the prompt appears on.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when no suitable client exists, when tmux refuses,
    /// and when the dispatch timeout expires before the prompt is answered.
    pub async fn command_prompt(
        &self,
        client: Option<&Client>,
        prompt: Option<&str>,
        command: impl Into<OsString>,
    ) -> Result<(), Error> {
        let mut request = Command::new("command-prompt");
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), "command-prompt")?;
            request = request
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }
        if let Some(prompt) = prompt {
            request = request.arg("-p").arg(OsString::from(prompt));
        }

        listing::mutate(&self.core, "command-prompt", request.arg(command.into())).await
    }

    /// Open one of tmux's interactive choosers on a client.
    ///
    /// A chooser opens *in a pane*, which is why this needs no client and
    /// succeeds on a server nothing is attached to: the pane's `pane_in_mode`
    /// becomes `1` and its `pane_mode` becomes `tree-mode`, client or not.
    ///
    /// That is the difference from [`Self::display_popup`],
    /// [`Self::display_menu`], [`Self::command_prompt`] and
    /// [`Self::display_panes`], which draw *on a client* and report "no current
    /// client" without one. Passing `client` here only says which client's
    /// current pane to open in.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when tmux refuses the command. A missing client is
    /// not one: this does not need one.
    pub async fn choose(&self, chooser: Chooser, client: Option<&Client>) -> Result<(), Error> {
        let name = chooser.command();
        let mut request = Command::new(name);
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), name)?;
            request = request
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }

        listing::mutate(&self.core, name, request).await
    }

    /// Open tmux's window finder for a search string.
    ///
    /// This is separate from [`Server::choose`] because it needs something to
    /// search for, where the other choosers list what already exists. Like them
    /// it opens in a pane rather than on a client, so it needs no client and
    /// leaves the pane in `tree-mode` on a server nothing is attached to.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when tmux refuses the command. A missing client is
    /// not one: this does not need one.
    pub async fn find_window(&self, client: Option<&Client>, search: &str) -> Result<(), Error> {
        let mut request = Command::new("find-window");
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), "find-window")?;
            request = request
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }

        listing::mutate(
            &self.core,
            "find-window",
            request.arg(OsString::from(search)),
        )
        .await
    }

    /// Briefly show each pane's number on a client.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ServerMismatch`] when the client belongs to another
    /// server, or an error when no suitable client exists or tmux refuses.
    pub async fn display_panes(&self, client: Option<&Client>) -> Result<(), Error> {
        let mut request = Command::new("display-panes");
        if let Some(client) = client {
            self.core
                .require_same_server(client.server_identity(), "display-panes")?;
            request = request
                .arg("-t")
                .arg(client.name().to_string_lossy().into_owned());
        }

        listing::mutate(&self.core, "display-panes", request).await
    }
}
