use std::collections::BTreeMap;

use clap::ArgMatches;
use regex::Regex;
use serde_json::{Value, json};

use super::{CliError, Result, document};

fn field(name: &str) -> Option<&'static str> {
    match name {
        "name" => Some("name"),
        "session" | "s" => Some("session"),
        "path" | "p" => Some("path"),
        "window" | "w" => Some("window"),
        "pane" => Some("pane"),
        _ => None,
    }
}

pub(super) struct Query {
    patterns: Vec<(Option<String>, Regex)>,
    fields: Vec<String>,
    any: bool,
    invert: bool,
}

impl Query {
    pub(super) fn new(args: &ArgMatches) -> Result<Self> {
        let terms: Vec<_> = args
            .get_many::<String>("query_terms")
            .into_iter()
            .flatten()
            .collect();
        if terms.is_empty() {
            return Err(CliError::usage("search requires at least one query term"));
        }
        let fields = args
            .get_many::<String>("field")
            .into_iter()
            .flatten()
            .flat_map(|value| value.split(','))
            .map(|value| {
                field(value)
                    .map(str::to_owned)
                    .ok_or_else(|| CliError::usage(format!("unknown search field {value:?}")))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut patterns = Vec::new();
        for term in terms {
            let (restriction, pattern) = term
                .split_once(':')
                .and_then(|(key, value)| field(key).map(|key| (Some(key.to_owned()), value)))
                .unwrap_or((None, term.as_str()));
            let insensitive = args.get_flag("ignore-case")
                || (args.get_flag("smart-case") && !pattern.chars().any(char::is_uppercase));
            let mut pattern = if args.get_flag("fixed-strings") {
                escape(pattern)
            } else {
                pattern.to_owned()
            };
            if args.get_flag("word-regexp") {
                pattern = format!(r"\b(?:{pattern})\b");
            }
            if insensitive {
                pattern = format!("(?i){pattern}");
            }
            let regex = Regex::new(&pattern)
                .map_err(|e| CliError::usage(format!("invalid pattern: {e}")))?;
            patterns.push((restriction, regex));
        }
        Ok(Self {
            patterns,
            fields,
            any: args.get_flag("any"),
            invert: args.get_flag("invert-match"),
        })
    }

    pub(super) fn run(&self, records: Vec<Value>) -> Vec<Value> {
        let mut results = Vec::new();
        for record in records {
            if record.get("error").is_some() {
                continue;
            }
            let mut fields: BTreeMap<&str, Vec<String>> = BTreeMap::new();
            for (name, key) in [
                ("name", "name"),
                ("path", "path"),
                ("session", "session_name"),
            ] {
                fields.insert(
                    name,
                    record
                        .get(key)
                        .and_then(Value::as_str)
                        .map(|v| vec![v.to_owned()])
                        .unwrap_or_default(),
                );
            }
            if let Some(windows) = record["config"]["windows"].as_array() {
                for window in windows {
                    if let Some(name) = window["window_name"].as_str() {
                        fields.entry("window").or_default().push(name.into());
                    }
                    if let Some(panes) = window["panes"].as_array() {
                        for pane in panes {
                            fields
                                .entry("pane")
                                .or_default()
                                .push(document::scalar(pane.get("shell_command").unwrap_or(pane)));
                        }
                    }
                }
            }
            let mut matches = BTreeMap::<String, Vec<String>>::new();
            let mut outcomes = Vec::new();
            for (restriction, pattern) in &self.patterns {
                let mut found = false;
                for (name, values) in &fields {
                    if restriction.as_deref().is_some_and(|f| f != *name)
                        || (!self.fields.is_empty() && !self.fields.iter().any(|f| f == name))
                    {
                        continue;
                    }
                    for value in values {
                        if pattern.is_match(value) {
                            found = true;
                            let values = matches.entry((*name).to_owned()).or_default();
                            if !values.contains(value) {
                                values.push(value.clone());
                            }
                        }
                    }
                }
                outcomes.push(found);
            }
            let selected = if self.any {
                outcomes.iter().any(|v| *v)
            } else {
                outcomes.iter().all(|v| *v)
            };
            if selected != self.invert {
                results.push(json!({"name":record["name"],"path":record["path"],"session_name":record["session_name"],"source":record["source"],"matched_fields":matches.keys().collect::<Vec<_>>(),"matches":matches}));
            }
        }
        results
    }
}

fn escape(text: &str) -> String {
    let mut output = String::new();
    for ch in text.chars() {
        if "\\.^$|?*+()[]{}".contains(ch) {
            output.push('\\');
        }
        output.push(ch);
    }
    output
}
