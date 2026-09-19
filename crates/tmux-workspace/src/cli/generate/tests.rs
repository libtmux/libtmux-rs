#![allow(clippy::unwrap_used)]

use super::*;

#[test]
fn metadata_reflects_aliases_arity_groups_and_declared_bindings() {
    let mut command = Command::new("fixture")
        .alias("hidden")
        .visible_alias("shown")
        .arg(Arg::new("items").num_args(2..=3).required(true))
        .arg(
            Arg::new("mode")
                .long("mode")
                .short('m')
                .alias("secret")
                .visible_alias("visible")
                .short_alias('q')
                .visible_short_alias('v')
                .env("CLI_GENERATE_TEST_MODE"),
        )
        .arg(Arg::new("other").long("other"))
        .group(
            clap::ArgGroup::new("choice")
                .args(["mode", "other"])
                .required(true)
                .multiple(true),
        );
    command.build();
    let metadata = metadata(&command, &[], &[]);
    assert_eq!(metadata["aliases"], json!(["hidden", "shown"]));
    assert_eq!(metadata["visible_aliases"], json!(["shown"]));
    let arguments = metadata["arguments"].as_array().unwrap();
    let items = arguments.iter().find(|v| v["name"] == "items").unwrap();
    assert_eq!(items["arity"], json!({"min":2,"max":3}));
    assert_eq!(items["index"], 1);
    let mode = arguments.iter().find(|v| v["name"] == "mode").unwrap();
    assert_eq!(
        mode["aliases"],
        json!({"long":["secret","visible"],"visible_long":["visible"],"short":["q","v"],"visible_short":["v"]})
    );
    assert_eq!(mode["environment"], "CLI_GENERATE_TEST_MODE");
    assert_eq!(
        metadata["groups"][0],
        json!({"name":"choice","members":["mode","other"],"required":true,"multiple":true})
    );
}

#[test]
fn declared_overrides_match_both_parser_orders() {
    for pair in [
        ["use-pythonrc", "no-startup"],
        ["use-vi-mode", "no-vi-mode"],
    ] {
        for [first, last] in [pair, [pair[1], pair[0]]] {
            let (command, declarations) = args::declared_command();
            let fact = declarations
                .iter()
                .find(|fact| fact.command == ["shell"] && fact.name == first)
                .unwrap();
            assert_eq!(fact.overrides, [last]);
            let matches = command
                .try_get_matches_from([
                    "tmux-workspace".to_owned(),
                    "shell".to_owned(),
                    format!("--{first}"),
                    format!("--{last}"),
                ])
                .unwrap();
            let shell = matches.subcommand_matches("shell").unwrap();
            assert!(!shell.get_flag(first));
            assert!(shell.get_flag(last));
        }
    }
}
