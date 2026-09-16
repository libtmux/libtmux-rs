mod environment;
#[cfg(test)]
mod tests;

use std::io::{self, Write};

use clap::{Arg, ArgAction, Command};
use serde_json::{Value, json};

use super::{CliError, Result, args, output::Reporter};

fn action_name(action: &ArgAction) -> &'static str {
    match action {
        ArgAction::Set => "Set",
        ArgAction::Append => "Append",
        ArgAction::SetTrue => "SetTrue",
        ArgAction::SetFalse => "SetFalse",
        ArgAction::Count => "Count",
        ArgAction::Help => "Help",
        ArgAction::HelpShort => "HelpShort",
        ArgAction::HelpLong => "HelpLong",
        ArgAction::Version => "Version",
        _ => "Unknown",
    }
}

fn argument(
    command: &Command,
    arg: &Arg,
    path: &[&str],
    declarations: &[args::DeclaredArgument],
) -> Value {
    let declared = declarations
        .iter()
        .filter(|fact| {
            fact.command == path.get(1..).unwrap_or_default() && fact.name == arg.get_id().as_str()
        })
        .collect::<Vec<_>>();
    let arity = arg.get_num_args().map(|range| {
        json!({"min":range.min_values(), "max":(range.max_values() != usize::MAX).then_some(range.max_values())})
    });
    json!({
        "name":arg.get_id().as_str(),
        "short":arg.get_short().map(|s| s.to_string()),
        "long":arg.get_long(),
        "aliases":{
            "long":arg.get_all_aliases().unwrap_or_default(),
            "visible_long":arg.get_visible_aliases().unwrap_or_default(),
            "short":arg.get_all_short_aliases().unwrap_or_default(),
            "visible_short":arg.get_visible_short_aliases().unwrap_or_default(),
        },
        "description":arg.get_help().map(ToString::to_string),
        "required":arg.is_required_set(),
        "global":arg.is_global_set(),
        "positional":arg.is_positional(),
        "index":arg.get_index(),
        "arity":arity,
        "action":action_name(arg.get_action()),
        "defaults":arg.get_default_values().iter().map(|v| v.to_string_lossy()).collect::<Vec<_>>(),
        "choices":arg.get_value_parser().possible_values().map(|v| v.map(|v| v.get_name().to_owned()).collect::<Vec<_>>()),
        "value_names":arg.get_value_names().map(|names| names.iter().map(clap::builder::Str::as_str).collect::<Vec<_>>()),
        "value_delimiter":arg.get_value_delimiter(),
        "value_terminator":arg.get_value_terminator().map(clap::builder::Str::as_str),
        "allow_negative_numbers":arg.is_allow_negative_numbers_set(),
        "allow_hyphen_values":arg.is_allow_hyphen_values_set(),
        "require_equals":arg.is_require_equals_set(),
        "trailing_var_arg":arg.is_trailing_var_arg_set(),
        "last":arg.is_last_set(),
        "hidden":arg.is_hide_set(),
        "environment":arg.get_env().map(|name| name.to_string_lossy()),
        "conflicts":command.get_arg_conflicts_with(arg).iter().map(|arg| arg.get_id().as_str()).collect::<Vec<_>>(),
        "overrides":declared.iter().flat_map(|fact| &fact.overrides).collect::<Vec<_>>(),
        "numeric_bounds":declared.iter().find_map(|fact| fact.numeric_bounds)
            .map(|(minimum, maximum)| json!({"minimum":minimum,"maximum":maximum,"inclusive":true,"source":"argument declaration"})),
    })
}

fn metadata(command: &Command, parent: &[&str], declarations: &[args::DeclaredArgument]) -> Value {
    let mut path = parent.to_vec();
    path.push(command.get_name());
    json!({
        "name":command.get_name(),
        "path":path,
        "description":command.get_about().map(ToString::to_string),
        "aliases":command.get_all_aliases().collect::<Vec<_>>(),
        "visible_aliases":command.get_visible_aliases().collect::<Vec<_>>(),
        "subcommand_required":command.is_subcommand_required_set(),
        "args_override_self":command.is_args_override_self(),
        "arguments":command.get_arguments().map(|arg| argument(command, arg, &path, declarations)).collect::<Vec<_>>(),
        "groups":command.get_groups().map(|group| json!({
            "name":group.get_id().as_str(),
            "members":group.get_args().map(clap::Id::as_str).collect::<Vec<_>>(),
            "required":group.is_required_set(),
            "multiple":group.clone().is_multiple(),
        })).collect::<Vec<_>>(),
        "subcommands":command.get_subcommands().map(|child| metadata(child, &path, declarations)).collect::<Vec<_>>()
    })
}

fn manual(command: &Command, parent: &[&str], output: &mut dyn Write) -> Result<()> {
    let mut path = parent.to_vec();
    path.push(command.get_name());
    let man = clap_mangen::Man::new(command.clone().bin_name(path.join(" ")));
    if parent.is_empty() {
        man.render_title(output)?;
        man.render_name_section(output)?;
        man.render_synopsis_section(output)?;
        man.render_description_section(output)?;
        man.render_options_section(output)?;
        man.render_version_section(output)?;
    } else {
        writeln!(output, ".SH \"{}\"", path.join(" "))?;
        let mut section = Vec::new();
        man.render_synopsis_section(&mut section)?;
        man.render_description_section(&mut section)?;
        man.render_options_section(&mut section)?;
        // Native section headings become subsections beneath this command.
        for line in section.split_inclusive(|byte| *byte == b'\n') {
            if let Some(heading) = line.strip_prefix(b".SH ") {
                output.write_all(b".SS ")?;
                output.write_all(heading)?;
            } else {
                output.write_all(line)?;
            }
        }
    }
    for child in command
        .get_subcommands()
        .filter(|child| !child.is_hide_set())
    {
        manual(child, &path, output)?;
    }
    Ok(())
}

fn render(
    format: &str,
    command: &mut Command,
    declarations: &[args::DeclaredArgument],
    output: &mut dyn Write,
) -> Result<()> {
    if format == "schema" {
        serde_json::to_writer(
            &mut *output,
            &json!({
                "schema_version":1,
                "command":metadata(command, &[], declarations),
                "metadata_sources":{"parser":"clap public reflection", "overrides":"argument declaration", "numeric_bounds":"argument declaration", "runtime_environment":"explicit runtime supplement"},
                "runtime_environment":environment::rules(),
            }),
        )?;
        writeln!(output)?;
    } else if format == "man" {
        manual(command, &[], output)?;
    } else {
        let shell = format
            .parse::<clap_complete::Shell>()
            .map_err(CliError::usage)?;
        if shell == clap_complete::Shell::Bash {
            let mut buffer = Vec::new();
            clap_complete::generate(shell, command, "tmux-workspace", &mut buffer);
            let script = String::from_utf8(buffer)
                .map_err(|error| CliError::new("encoding", error.to_string()))?;
            output.write_all(bash_hyphenated_case_labels("tmux-workspace", &script).as_bytes())?;
        } else {
            clap_complete::generate(shell, command, "tmux-workspace", output);
        }
    }
    Ok(())
}

/// `clap_complete` 4.6.9's bash generator mangles a hyphenated bin name two
/// different ways: the routing table (`cmd="..."`) replaces `-` with `__`,
/// but each `case` label's path is built from the full space-joined bin name
/// and replaces every `-` with `__subcmd__`, including the one in the bin
/// name itself. The two spellings never meet, so every subcommand branch
/// this generates is unreachable. Rewrite the labels back to the routed
/// spelling; every other line already matches.
fn bash_hyphenated_case_labels(bin_name: &str, script: &str) -> String {
    let routed = bin_name.replace('-', "__");
    let mislabeled = bin_name.replace('-', "__subcmd__");
    if routed == mislabeled {
        script.to_owned()
    } else {
        script.replace(&mislabeled, &routed)
    }
}

pub(super) fn write(format: &str, report: &mut Reporter) -> Result<()> {
    let (mut command, declarations) = args::declared_command();
    command.build();
    if !report.machine() {
        let mut output = io::stdout().lock();
        render(format, &mut command, &declarations, &mut output)?;
        output.flush()?;
        return Ok(());
    }
    let mut output = Vec::new();
    render(format, &mut command, &declarations, &mut output)?;
    let content =
        String::from_utf8(output).map_err(|error| CliError::new("encoding", error.to_string()))?;
    report.summary(
        "completed",
        &json!({
            "schema_version":1,"command":"generate","status":"ok",
            "artifact":{"format":format,"encoding":"utf-8", "content":content},
        }),
    )
}
