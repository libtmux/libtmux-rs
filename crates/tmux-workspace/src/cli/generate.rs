use std::io::{self, Write};

use clap::Command;
use serde_json::{Value, json};

use super::{CliError, Result, args};

fn metadata(command: &Command) -> Value {
    json!({
        "name":command.get_name(),
        "description":command.get_about().map(ToString::to_string),
        "arguments":command.get_arguments().map(|arg| json!({
            "name":arg.get_id().as_str(),
            "short":arg.get_short().map(|s| s.to_string()),
            "long":arg.get_long(),
            "description":arg.get_help().map(ToString::to_string),
            "required":arg.is_required_set(),
            "global":arg.is_global_set(),
            "positional":arg.is_positional(),
            "action":format!("{:?}", arg.get_action()),
            "defaults":arg.get_default_values().iter().map(|v| v.to_string_lossy()).collect::<Vec<_>>(),
            "choices":arg.get_value_parser().possible_values().map(|v| v.map(|v| v.get_name().to_owned()).collect::<Vec<_>>())
        })).collect::<Vec<_>>(),
        "subcommands":command.get_subcommands().map(metadata).collect::<Vec<_>>()
    })
}

pub(super) fn write(format: &str) -> Result<()> {
    let mut command = args::command();
    command.build();
    let mut output = io::stdout().lock();
    if format == "schema" {
        serde_json::to_writer(
            &mut output,
            &json!({"schema_version":1,"command":metadata(&command)}),
        )?;
        writeln!(output)?;
    } else if format == "man" {
        clap_mangen::Man::new(command).render(&mut output)?;
    } else {
        let shell = format
            .parse::<clap_complete::Shell>()
            .map_err(CliError::usage)?;
        clap_complete::generate(shell, &mut command, "tmux-workspace", &mut output);
    }
    output.flush()?;
    Ok(())
}
