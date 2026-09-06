//! Format-preserving configuration contract tests.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::Path;

use mcp_swap::config::{
    Action, ClientConfig, Scope, ServerSpec, delete_server, read_server, set_server,
};

#[test]
fn jsonc_rewrites_only_changed_members() {
    let original = br#"{
  // keep the document comment
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "other": { "type": "remote", "url": "https://example.test/mcp" },
    "tmux": {
      // keep the entry comment
      "type": "local",
      "command": ["old", "--flag"],
      "environment": {"KEEP": "yes"},
      "retired": true,
    },
  },
}
"#;
    let client = ClientConfig::for_name("opencode").expect("known client");
    let spec = ServerSpec {
        command: "/repo/target/debug/tmux-mcp".into(),
        args: vec!["--stdio".into()],
        env: BTreeMap::from([("KEEP".into(), "yes".into())]),
    };

    let edit = set_server(
        &client,
        original,
        "tmux",
        &spec,
        Path::new("/repo"),
        Scope::User,
    )
    .expect("valid JSONC");

    assert_eq!(edit.action, Action::Replaced);
    let rendered = String::from_utf8(edit.bytes.clone()).expect("UTF-8 config");
    assert!(rendered.contains("// keep the document comment"));
    assert!(rendered.contains("// keep the entry comment"));
    assert!(rendered.contains("\"other\": { \"type\": \"remote\""));
    assert!(!rendered.contains("retired"));
    assert_eq!(
        read_server(
            &client,
            &edit.bytes,
            "tmux",
            Path::new("/repo"),
            Scope::User
        )
        .expect("rendered JSONC")
        .expect("tmux entry"),
        spec
    );
}

#[test]
fn toml_preserves_unrelated_tables_and_comments() {
    let original = br#"# keep the leading comment
theme = "dark"

[mcp_servers.other]
command = "other"
args = ["--keep"]

# keep the tmux table comment
[mcp_servers.tmux]
command = "old"
args = []

[mcp_servers.tmux.env]
KEEP = "yes"

# keep the trailing comment
"#;
    let client = ClientConfig::for_name("codex").expect("known client");
    let spec = ServerSpec {
        command: "/repo/target/release/tmux-mcp".into(),
        args: Vec::new(),
        env: BTreeMap::from([("KEEP".into(), "yes".into())]),
    };

    let edit = set_server(
        &client,
        original,
        "tmux",
        &spec,
        Path::new("/repo"),
        Scope::User,
    )
    .expect("valid TOML");
    let rendered = String::from_utf8(edit.bytes.clone()).expect("UTF-8 config");

    assert!(rendered.starts_with("# keep the leading comment\n"));
    assert!(rendered.contains("[mcp_servers.other]\ncommand = \"other\""));
    assert!(rendered.contains("# keep the tmux table comment"));
    assert!(rendered.ends_with("# keep the trailing comment\n"));
    assert_eq!(
        read_server(
            &client,
            &edit.bytes,
            "tmux",
            Path::new("/repo"),
            Scope::User
        )
        .expect("rendered TOML")
        .expect("tmux entry"),
        spec
    );
}

#[test]
fn toml_preserves_an_inline_container_and_neighbor() {
    let original = br#"# retained
mcp_servers = { other = { command = "other" }, tmux = { command = "old", args = [] } } # keep inline
"#;
    let client = ClientConfig::for_name("codex").expect("known client");
    let spec = ServerSpec {
        command: "tmux-mcp".into(),
        args: vec!["--stdio".into()],
        env: BTreeMap::from([("KEEP".into(), "yes".into())]),
    };

    let edit = set_server(
        &client,
        original,
        "tmux",
        &spec,
        Path::new("/repo"),
        Scope::User,
    )
    .expect("inline TOML");
    let rendered = String::from_utf8(edit.bytes.clone()).expect("UTF-8 TOML");

    assert!(rendered.starts_with("# retained\nmcp_servers = {"));
    assert!(rendered.contains("other = { command = \"other\" }"));
    assert!(rendered.contains("# keep inline"));
    assert_eq!(
        read_server(
            &client,
            &edit.bytes,
            "tmux",
            Path::new("/repo"),
            Scope::User,
        )
        .expect("rendered TOML"),
        Some(spec)
    );
}

#[test]
fn toml_updates_dotted_target_assignments_without_rewriting_neighbors() {
    let original = br#"# retained
mcp_servers.other.command = "other"
mcp_servers.tmux.command = "old"
mcp_servers.tmux.args = []
theme = "dark"
"#;
    let client = ClientConfig::for_name("grok").expect("known client");
    let spec = ServerSpec {
        command: "tmux-mcp".into(),
        args: vec!["--stdio".into()],
        env: BTreeMap::new(),
    };

    let edit = set_server(
        &client,
        original,
        "tmux",
        &spec,
        Path::new("/repo"),
        Scope::User,
    )
    .expect("dotted TOML");
    let rendered = String::from_utf8(edit.bytes.clone()).expect("UTF-8 TOML");

    assert!(rendered.starts_with("# retained\n"));
    assert!(rendered.contains("mcp_servers.other.command = \"other\""));
    assert!(rendered.contains("theme = \"dark\""));
    assert_eq!(
        read_server(
            &client,
            &edit.bytes,
            "tmux",
            Path::new("/repo"),
            Scope::User,
        )
        .expect("rendered TOML"),
        Some(spec)
    );
}

#[test]
fn claude_scopes_are_independent_and_preserve_unrelated_json() {
    let original = br#"{
    "theme": "dark",
    "mcpServers": {"tmux": {"type":"stdio","command":"user","args":[],"env":{}}},
    "projects": {
        "/repo": {"allowedTools": ["Read"], "mcpServers": {}}
    }
}"#;
    let client = ClientConfig::for_name("claude").expect("known client");
    let spec = ServerSpec {
        command: "/repo/target/debug/tmux-mcp".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };

    let edit = set_server(
        &client,
        original,
        "tmux",
        &spec,
        Path::new("/repo"),
        Scope::Project,
    )
    .expect("valid Claude JSON");

    assert_eq!(
        read_server(
            &client,
            &edit.bytes,
            "tmux",
            Path::new("/repo"),
            Scope::User
        )
        .expect("valid user scope")
        .expect("user entry")
        .command,
        "user"
    );
    assert_eq!(
        read_server(
            &client,
            &edit.bytes,
            "tmux",
            Path::new("/repo"),
            Scope::Project,
        )
        .expect("valid project scope")
        .expect("project entry"),
        spec
    );
    assert!(
        String::from_utf8(edit.bytes)
            .expect("UTF-8")
            .contains("\"allowedTools\"")
    );
}

#[test]
fn setting_an_identical_entry_is_byte_identical() {
    let original = b"{\"mcpServers\":{\"tmux\":{\"command\":\"tmux-mcp\",\"args\":[]}}}\n";
    let client = ClientConfig::for_name("cursor").expect("known client");
    let spec = ServerSpec {
        command: "tmux-mcp".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };

    let edit = set_server(
        &client,
        original,
        "tmux",
        &spec,
        Path::new("/repo"),
        Scope::User,
    )
    .expect("valid JSON");

    assert_eq!(edit.bytes, original);
}

#[test]
fn empty_opencode_config_seeds_schema_and_local_dialect() {
    let client = ClientConfig::for_name("opencode").expect("known client");
    let spec = ServerSpec {
        command: "tmux-mcp".into(),
        args: vec!["--stdio".into()],
        env: BTreeMap::new(),
    };

    let edit = set_server(&client, b"", "tmux", &spec, Path::new("/repo"), Scope::User)
        .expect("empty config");
    let rendered = String::from_utf8(edit.bytes).expect("UTF-8");

    assert!(rendered.contains("https://opencode.ai/config.json"));
    assert!(rendered.contains("\"type\": \"local\""));
    assert!(rendered.contains("\"command\": ["));
}

#[test]
fn whitespace_only_json_is_treated_as_an_empty_config() {
    let client = ClientConfig::for_name("cursor").expect("known client");
    let spec = ServerSpec {
        command: "tmux-mcp".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };

    let edit = set_server(
        &client,
        b" \n\t",
        "tmux",
        &spec,
        Path::new("/repo"),
        Scope::User,
    )
    .expect("blank config");

    assert_eq!(
        read_server(
            &client,
            &edit.bytes,
            "tmux",
            Path::new("/repo"),
            Scope::User,
        )
        .expect("rendered JSON"),
        Some(spec)
    );
    assert!(edit.bytes.ends_with(b"\n"));
}

#[test]
fn jsonc_comment_only_container_and_trailing_comma_survive() {
    let original = br#"{
  "mcp": {
    // no servers yet because this profile is offline
  },
  "label": "cafe \u2615",
}
"#;
    let client = ClientConfig::for_name("opencode").expect("known client");
    let spec = ServerSpec {
        command: "tmux-mcp".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };

    let edit = set_server(
        &client,
        original,
        "tmux",
        &spec,
        Path::new("/repo"),
        Scope::User,
    )
    .expect("valid JSONC");
    let rendered = String::from_utf8(edit.bytes).expect("UTF-8");

    assert!(rendered.contains("// no servers yet because this profile is offline"));
    assert!(rendered.contains("\"label\": \"cafe \\u2615\""));
    assert!(rendered.contains("\n  },\n"));
}

#[test]
fn escaped_server_key_is_written_and_removed_semantically() {
    let original = b"{\n  \"mcp\": {}\n}";
    let client = ClientConfig::for_name("opencode").expect("known client");
    let spec = ServerSpec {
        command: "tmux-mcp".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    let server = "quo\"te\\path\nline";

    let changed = set_server(
        &client,
        original,
        server,
        &spec,
        Path::new("/repo"),
        Scope::User,
    )
    .expect("escaped key");
    assert_eq!(
        read_server(
            &client,
            &changed.bytes,
            server,
            Path::new("/repo"),
            Scope::User,
        )
        .expect("read escaped key"),
        Some(spec)
    );

    let removed = delete_server(
        &client,
        &changed.bytes,
        server,
        Path::new("/repo"),
        Scope::User,
    )
    .expect("remove escaped key");
    assert_eq!(removed.action, Action::Removed);
    assert_eq!(
        read_server(
            &client,
            &removed.bytes,
            server,
            Path::new("/repo"),
            Scope::User,
        )
        .expect("read removed key"),
        None
    );
}

#[test]
fn malformed_container_is_refused_without_rewriting() {
    let original = br#"{"mcpServers": []}"#;
    let client = ClientConfig::for_name("cursor").expect("known client");
    let spec = ServerSpec {
        command: "tmux-mcp".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };

    let error = set_server(
        &client,
        original,
        "tmux",
        &spec,
        Path::new("/repo"),
        Scope::User,
    )
    .expect_err("array container must fail closed");

    assert!(error.to_string().contains("mcpServers must be an object"));
}

#[test]
fn duplicate_json_jsonc_and_toml_containers_are_rejected() {
    let spec = ServerSpec {
        command: "tmux-mcp".into(),
        args: Vec::new(),
        env: BTreeMap::new(),
    };
    for (name, original) in [
        (
            "cursor",
            br#"{"mcpServers":{},"mcpServers":{"tmux":{"command":"old"}}}"#.as_slice(),
        ),
        (
            "opencode",
            br#"{"mcp":{},/* ambiguous */"mcp":{"tmux":{"type":"local","command":["old"]}}}"#
                .as_slice(),
        ),
        (
            "codex",
            b"mcp_servers = {}\nmcp_servers = { tmux = { command = \"old\" } }\n".as_slice(),
        ),
    ] {
        let client = ClientConfig::for_name(name).expect("known client");
        let error = set_server(
            &client,
            original,
            "tmux",
            &spec,
            Path::new("/repo"),
            Scope::User,
        )
        .expect_err("duplicate container must fail closed");
        assert!(error.to_string().contains("duplicate"), "{name}: {error}");
    }
}

#[test]
fn toml_delete_keeps_neighboring_table_and_comment() {
    let original = br#"# retained
[mcp_servers.other]
command = "other"

[mcp_servers.tmux]
command = "old"
args = []
"#;
    let client = ClientConfig::for_name("grok").expect("known client");

    let removed = delete_server(&client, original, "tmux", Path::new("/repo"), Scope::User)
        .expect("valid TOML");
    let rendered = String::from_utf8(removed.bytes).expect("UTF-8");

    assert!(rendered.starts_with("# retained\n"));
    assert!(rendered.contains("[mcp_servers.other]\ncommand = \"other\""));
    assert!(!rendered.contains("mcp_servers.tmux"));
}

#[test]
fn every_client_dialect_round_trips_one_server() {
    for name in [
        "claude", "codex", "cursor", "gemini", "grok", "agy", "opencode", "pi",
    ] {
        let client = ClientConfig::for_name(name).expect("known client");
        let spec = ServerSpec {
            command: "/repo/tmux-mcp".into(),
            args: vec!["--stdio".into()],
            env: BTreeMap::from([("TOKEN".into(), "value".into())]),
        };

        let changed = set_server(
            &client,
            b"",
            "tmux",
            &spec,
            Path::new("/repo"),
            Scope::Project,
        )
        .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            read_server(
                &client,
                &changed.bytes,
                "tmux",
                Path::new("/repo"),
                Scope::Project,
            )
            .unwrap_or_else(|error| panic!("{name}: {error}")),
            Some(spec),
            "{name}"
        );
    }
}
