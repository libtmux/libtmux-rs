//! One typed capability definition bound to each native MCP tool route.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rmcp::handler::server::router::tool::{ToolRoute, ToolRouter};
use rmcp::model::{MetaObject, ToolAnnotations};
use serde::{Deserialize, Serialize};

use crate::TmuxTools;
use crate::policy::{Selection, SurfaceError, Toolset};

pub(crate) const CAPABILITY_KEY: &str = "com.git-pull.libtmux-mcp/capability";
const DEFINITION_KEY: &str = "com.git-pull.libtmux-mcp/internal-definition";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ProcessReach {
    None,
    ConfiguredProcess,
    PaneInput,
    PaneCommand,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum TmuxEffect {
    Observe,
    Change,
    Delete,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum OutputClass {
    TmuxMetadata,
    TerminalContent,
    ProcessEnvironment,
    ConfiguredCommand,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum InputSink {
    None,
    TmuxLookup,
    TmuxState,
    TmuxFormat,
    PaneInput,
    ShellCommand,
    ProcessArgv,
    Regex,
    NestedTool,
}

#[cfg(test)]
mod input_sink_tests {
    use std::collections::BTreeSet;

    use super::InputSink;

    #[test]
    fn wire_vocabulary_is_exact() {
        let error = serde_json::from_str::<InputSink>(r#""not-a-sink""#)
            .expect_err("the sentinel must not be a valid input sink")
            .to_string();
        let (_, expected) = error
            .split_once("expected ")
            .expect("serde lists the accepted wire variants");
        let (expected, _) = expected
            .split_once(" at line ")
            .expect("serde reports the JSON location");
        let actual: BTreeSet<_> = expected
            .trim_start_matches("one of ")
            .split(", ")
            .map(|name| name.trim_matches('`'))
            .collect();
        let shared = BTreeSet::from([
            "nested-tool",
            "none",
            "pane-input",
            "process-argv",
            "regex",
            "shell-command",
            "tmux-format",
            "tmux-lookup",
            "tmux-state",
        ]);

        assert_eq!(actual, shared);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum InputLiteralization {
    DoubleHashOnce,
    ValidatedVariableName,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "MCP defines four independent annotation hints"
)]
pub(crate) struct Annotations {
    pub(crate) read_only_hint: bool,
    pub(crate) destructive_hint: bool,
    pub(crate) idempotent_hint: bool,
    pub(crate) open_world_hint: bool,
}

impl Annotations {
    pub(crate) const CONSERVATIVE: Self = Self {
        read_only_hint: false,
        destructive_hint: true,
        idempotent_hint: false,
        open_world_hint: true,
    };

    fn render(self, title: Option<String>) -> ToolAnnotations {
        ToolAnnotations::from_raw(
            title,
            Some(self.read_only_hint),
            Some(self.destructive_hint),
            Some(self.idempotent_hint),
            Some(self.open_world_hint),
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the capability contract carries independent boolean facts"
)]
pub(crate) struct Capability {
    pub(crate) toolset: Toolset,
    pub(crate) process_reach: ProcessReach,
    pub(crate) tmux_effects: BTreeSet<TmuxEffect>,
    pub(crate) output_classes: BTreeSet<OutputClass>,
    pub(crate) may_expose_secrets: bool,
    pub(crate) may_return_untrusted_content: bool,
    pub(crate) annotations: Annotations,
    pub(crate) input_sinks: BTreeMap<String, BTreeSet<InputSink>>,
    pub(crate) input_literalization: BTreeMap<String, InputLiteralization>,
    pub(crate) nested_authority: BTreeSet<String>,
    pub(crate) amplifies_future_input: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the public capability contract carries independent boolean facts"
)]
pub(crate) struct PublishedCapability {
    pub(crate) toolset: Toolset,
    pub(crate) process_reach: ProcessReach,
    pub(crate) tmux_effects: BTreeSet<TmuxEffect>,
    pub(crate) output_classes: BTreeSet<OutputClass>,
    pub(crate) may_expose_secrets: bool,
    pub(crate) may_return_untrusted_content: bool,
    pub(crate) annotations: Annotations,
    pub(crate) input_literalization: BTreeMap<String, InputLiteralization>,
    pub(crate) nested_authority: BTreeSet<String>,
    pub(crate) amplifies_future_input: bool,
}

impl From<&Capability> for PublishedCapability {
    fn from(definition: &Capability) -> Self {
        Self {
            toolset: definition.toolset,
            process_reach: definition.process_reach,
            tmux_effects: definition.tmux_effects.clone(),
            output_classes: definition.output_classes.clone(),
            may_expose_secrets: definition.may_expose_secrets,
            may_return_untrusted_content: definition.may_return_untrusted_content,
            annotations: definition.annotations,
            input_literalization: definition.input_literalization.clone(),
            nested_authority: definition.nested_authority.clone(),
            amplifies_future_input: definition.amplifies_future_input,
        }
    }
}

impl Capability {
    pub(crate) fn controlled_opener(&self) -> &'static str {
        controlled_opener(self.toolset, self.process_reach, &self.output_classes)
    }
}

fn controlled_opener(
    toolset: Toolset,
    process_reach: ProcessReach,
    output_classes: &BTreeSet<OutputClass>,
) -> &'static str {
    match process_reach {
        ProcessReach::ConfiguredProcess => {
            "Start a pane's configured process; accepts no command payload."
        }
        ProcessReach::PaneInput => {
            "Send input to a pane's program; a shell that receives it runs it with your user's permissions."
        }
        ProcessReach::PaneCommand => "Run a shell command in a pane with your user's permissions.",
        ProcessReach::None => match toolset {
            Toolset::Manage | Toolset::Execute => {
                "Change tmux state; no client-supplied executable input."
            }
            Toolset::Teardown => "Delete tmux state; accepts no command payload.",
            Toolset::Inspect if output_classes.contains(&OutputClass::TerminalContent) => {
                "Read pane output; accepts no client-supplied executable input. Returned content may be sensitive or untrusted."
            }
            Toolset::Inspect if output_classes.contains(&OutputClass::ProcessEnvironment) => {
                "Read the tmux environment; accepts no client-supplied executable input. Returned values may contain secrets."
            }
            Toolset::Inspect if output_classes.contains(&OutputClass::ConfiguredCommand) => {
                "Read configured tmux commands; accepts no client-supplied executable input. Returned values may contain executable configuration."
            }
            Toolset::Inspect => {
                "Inspect tmux metadata; accepts no client-supplied executable input."
            }
        },
    }
}

/// Build the namespaced custom metadata attached to one native tool route.
#[allow(
    clippy::expect_used,
    reason = "the closed capability value has no fallible serializer"
)]
pub(crate) fn metadata(capability: Capability, always_load: bool) -> MetaObject {
    let mut meta = MetaObject::new();
    meta.0.insert(
        DEFINITION_KEY.to_owned(),
        serde_json::to_value(capability).expect("capability metadata serializes"),
    );
    if always_load {
        meta.0.insert(
            "anthropic/alwaysLoad".to_owned(),
            serde_json::Value::Bool(true),
        );
    }
    meta
}

fn capability(meta: Option<&MetaObject>, name: &str) -> Result<Capability, SurfaceError> {
    let value = meta
        .and_then(|meta| meta.0.get(DEFINITION_KEY))
        .ok_or_else(|| SurfaceError::new(format!("tool {name:?} has no capability manifest")))?;
    serde_json::from_value(value.clone()).map_err(|error| {
        SurfaceError::new(format!(
            "tool {name:?} has invalid capability metadata: {error}"
        ))
    })
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReportTool {
    pub(crate) name: String,
    pub(crate) title: String,
    pub(crate) description: String,
    #[serde(flatten)]
    pub(crate) capability: PublishedCapability,
    pub(crate) input_schema: serde_json::Value,
    pub(crate) output_schema: serde_json::Value,
}

#[cfg(test)]
impl ReportTool {
    pub(crate) fn controlled_opener(&self) -> &'static str {
        controlled_opener(
            self.capability.toolset,
            self.capability.process_reach,
            &self.capability.output_classes,
        )
    }
}

/// The frozen, effective MCP surface reported at `tmux://capabilities`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityReport {
    pub(crate) schema_version: u8,
    pub(crate) frozen: bool,
    pub(crate) boundary: BoundaryReport,
    pub(crate) connection: ConnectionReport,
    pub(crate) socket: SocketReport,
    pub(crate) toolsets: Vec<&'static str>,
    pub(crate) included_tools: Vec<String>,
    pub(crate) excluded_tools: Vec<String>,
    pub(crate) tool_count: usize,
    pub(crate) effective_tools: Vec<String>,
    pub(crate) tools: Vec<ReportTool>,
    pub(crate) host_command_tools: u8,
    pub(crate) tool_filtering_boundary: &'static str,
    pub(crate) execution_authority: &'static str,
    pub(crate) operating_system_boundary: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the report carries four independent MCP boundary facts"
)]
pub(crate) struct BoundaryReport {
    pub(crate) one_socket_per_process: bool,
    pub(crate) per_call_socket_selection: bool,
    pub(crate) host_command_execution: bool,
    pub(crate) dynamic_resources: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionReport {
    pub(crate) socket_selector: String,
    pub(crate) socket_provenance: &'static str,
    pub(crate) resolved_socket_path: String,
    pub(crate) server_state: &'static str,
    pub(crate) configuration_provenance: &'static str,
    pub(crate) attach_command: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SocketReport {
    pub(crate) selector: String,
    pub(crate) selection_provenance: &'static str,
    pub(crate) server_state: &'static str,
    pub(crate) configuration_provenance: &'static str,
    pub(crate) namespace_boundary: &'static str,
}

pub(crate) struct Resolved {
    pub(crate) router: ToolRouter<TmuxTools>,
    pub(crate) nested_router: ToolRouter<TmuxTools>,
    pub(crate) report: CapabilityReport,
}

pub(crate) fn resolve(
    mut router: ToolRouter<TmuxTools>,
    selection: &Selection,
) -> Result<Resolved, SurfaceError> {
    let known: BTreeSet<String> = router.map.keys().map(ToString::to_string).collect();
    for name in selection
        .included_names()
        .iter()
        .chain(selection.excluded_names())
    {
        if !known.contains(name) {
            return Err(SurfaceError::new(format!("unknown tool {name:?}")));
        }
    }

    let mut capabilities = BTreeMap::new();
    for (name, route) in &mut router.map {
        let row = capability(route.attr.meta.as_ref(), name)?;
        let input_schema = Arc::make_mut(&mut route.attr.input_schema);
        input_schema.insert("additionalProperties".to_owned(), false.into());
        validate(name, input_schema, &row, &known)?;
        capabilities.insert(name.to_string(), row);
    }
    let source_capabilities = capabilities.clone();
    let mut nested_router = router.clone();

    let selected_toolsets: BTreeSet<_> = selection.toolsets().iter().copied().collect();
    let withheld: Vec<_> = capabilities
        .iter()
        .filter(|(name, row)| {
            (!selected_toolsets.contains(&row.toolset) && !selection.includes(name))
                || selection.excludes(name)
        })
        .map(|(name, _)| name.clone())
        .collect();
    for name in withheld {
        router.remove_route(&name);
        capabilities.remove(&name);
    }

    for row in capabilities.values_mut() {
        if !row.nested_authority.is_empty() {
            row.nested_authority
                .retain(|name| !selection.excludes(name));
            recompute_aggregate(row, &source_capabilities)?;
        }
    }
    let nested_authority: BTreeSet<_> = capabilities
        .values()
        .flat_map(|row| row.nested_authority.iter().cloned())
        .collect();
    nested_router
        .map
        .retain(|name, _| nested_authority.contains(name.as_ref()));

    let mut tools = Vec::with_capacity(capabilities.len());
    for (name, row) in capabilities {
        let route = router
            .map
            .get_mut(name.as_str())
            .ok_or_else(|| SurfaceError::new(format!("tool {name:?} has no route")))?;
        let report = finish_route(name.clone(), &row, route, &nested_router)?;
        refresh_metadata(route.attr.meta.as_mut(), name.as_str(), &report)?;
        tools.push(report);
    }

    Ok(Resolved {
        router,
        nested_router,
        report: CapabilityReport {
            schema_version: 1,
            frozen: true,
            boundary: BoundaryReport {
                one_socket_per_process: true,
                per_call_socket_selection: false,
                host_command_execution: false,
                dynamic_resources: false,
            },
            connection: ConnectionReport {
                socket_selector: String::new(),
                socket_provenance: "unknown",
                resolved_socket_path: String::new(),
                server_state: "unknown",
                configuration_provenance: "unknown",
                attach_command: String::new(),
            },
            socket: SocketReport {
                selector: String::new(),
                selection_provenance: "unknown",
                server_state: "unknown",
                configuration_provenance: "unknown",
                namespace_boundary: "tmux-objects-only",
            },
            toolsets: selection
                .toolsets()
                .iter()
                .map(|toolset| toolset.name())
                .collect(),
            included_tools: selection.included_names().iter().cloned().collect(),
            excluded_tools: selection.excluded_names().iter().cloned().collect(),
            tool_count: tools.len(),
            effective_tools: tools.iter().map(|tool| tool.name.clone()).collect(),
            tools,
            host_command_tools: 0,
            tool_filtering_boundary: "interface-shaping-not-authorization",
            execution_authority: "tmux-user",
            operating_system_boundary: "none",
        },
    })
}

fn finish_route(
    name: String,
    row: &Capability,
    route: &mut ToolRoute<TmuxTools>,
    nested_router: &ToolRouter<TmuxTools>,
) -> Result<ReportTool, SurfaceError> {
    let opener = row.controlled_opener();
    let remainder = route.attr.description.as_deref().unwrap_or("").trim();
    route.attr.description = Some(
        if remainder.starts_with(opener) {
            remainder.to_owned()
        } else if remainder.is_empty() {
            opener.to_owned()
        } else {
            format!("{opener} {remainder}")
        }
        .into(),
    );
    route.attr.annotations = Some(row.annotations.render(route.attr.title.clone()));
    if row
        .input_sinks
        .values()
        .any(|sinks| sinks.contains(&InputSink::NestedTool))
    {
        set_nested_operation_schemas(
            &name,
            Arc::make_mut(&mut route.attr.input_schema),
            nested_router,
        )?;
    }
    let title = route
        .attr
        .title
        .as_deref()
        .ok_or_else(|| SurfaceError::new(format!("tool {name:?} has no title")))?
        .to_owned();
    let description = route
        .attr
        .description
        .as_deref()
        .ok_or_else(|| SurfaceError::new(format!("tool {name:?} has no description")))?
        .to_owned();
    let output_schema = route
        .attr
        .output_schema
        .as_ref()
        .ok_or_else(|| SurfaceError::new(format!("tool {name:?} has no output schema")))?;
    let capability = PublishedCapability::from(row);
    Ok(ReportTool {
        name,
        title,
        description,
        capability,
        input_schema: serde_json::Value::Object((*route.attr.input_schema).clone()),
        output_schema: serde_json::Value::Object((**output_schema).clone()),
    })
}

fn recompute_aggregate(
    aggregate: &mut Capability,
    source: &BTreeMap<String, Capability>,
) -> Result<(), SurfaceError> {
    aggregate.tmux_effects.clear();
    aggregate.output_classes.clear();
    aggregate.may_expose_secrets = false;
    aggregate.may_return_untrusted_content = false;
    for name in &aggregate.nested_authority {
        let nested = source
            .get(name)
            .ok_or_else(|| SurfaceError::new(format!("unknown nested tool {name:?}")))?;
        aggregate
            .tmux_effects
            .extend(nested.tmux_effects.iter().copied());
        aggregate
            .output_classes
            .extend(nested.output_classes.iter().copied());
        aggregate.may_expose_secrets |= nested.may_expose_secrets;
        aggregate.may_return_untrusted_content |= nested.may_return_untrusted_content;
    }
    if aggregate.nested_authority.is_empty() {
        aggregate.tmux_effects.insert(TmuxEffect::Observe);
    }
    Ok(())
}

fn refresh_metadata(
    meta: Option<&mut MetaObject>,
    name: &str,
    row: &ReportTool,
) -> Result<(), SurfaceError> {
    let meta = meta.ok_or_else(|| SurfaceError::new(format!("tool {name:?} has no metadata")))?;
    let value = serde_json::to_value(row).map_err(|error| {
        SurfaceError::new(format!(
            "tool {name:?} capability cannot serialize: {error}"
        ))
    })?;
    meta.0.remove(DEFINITION_KEY);
    meta.0.insert(CAPABILITY_KEY.to_owned(), value);
    Ok(())
}

fn set_nested_operation_schemas(
    name: &str,
    schema: &mut serde_json::Map<String, serde_json::Value>,
    nested: &ToolRouter<TmuxTools>,
) -> Result<(), SurfaceError> {
    let operations = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
        .and_then(|properties| properties.get_mut("operations"))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| SurfaceError::new(format!("tool {name:?} has no operations schema")))?;
    let items = if nested.map.is_empty() {
        serde_json::json!({"not": {}})
    } else {
        let mut names = nested
            .map
            .keys()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        names.sort();
        let alternatives = names
            .into_iter()
            .map(|nested_name| {
                let route = nested.map.get(nested_name.as_str()).ok_or_else(|| {
                    SurfaceError::new(format!("unknown nested tool {nested_name:?}"))
                })?;
                let input = inline_local_references(serde_json::Value::Object(
                    (*route.attr.input_schema).clone(),
                ))?;
                Ok(serde_json::json!({
                    "type": "object",
                    "properties": {
                        "tool": {"type": "string", "const": nested_name},
                        "arguments": input,
                    },
                    "required": ["tool"],
                    "additionalProperties": false,
                }))
            })
            .collect::<Result<Vec<_>, SurfaceError>>()?;
        serde_json::json!({"oneOf": alternatives})
    };
    operations.insert("items".to_owned(), items);
    if let Some(definitions) = schema
        .get_mut("$defs")
        .and_then(serde_json::Value::as_object_mut)
    {
        definitions.remove("ReadOperation");
        if definitions.is_empty() {
            schema.remove("$defs");
        }
    }
    Ok(())
}

fn inline_local_references(
    mut schema: serde_json::Value,
) -> Result<serde_json::Value, SurfaceError> {
    let definitions = schema
        .get("$defs")
        .and_then(serde_json::Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(object) = schema.as_object_mut() {
        object.remove("$defs");
    }
    inline_references_in(&mut schema, &definitions, 0)?;
    Ok(schema)
}

fn inline_references_in(
    value: &mut serde_json::Value,
    definitions: &serde_json::Map<String, serde_json::Value>,
    depth: usize,
) -> Result<(), SurfaceError> {
    if depth > 32 {
        return Err(SurfaceError::new(
            "nested input schema reference depth exceeded",
        ));
    }
    if let Some(reference) = value
        .as_object()
        .and_then(|object| object.get("$ref"))
        .and_then(serde_json::Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
    {
        *value = definitions
            .get(reference)
            .cloned()
            .ok_or_else(|| SurfaceError::new(format!("unknown schema reference {reference:?}")))?;
        return inline_references_in(value, definitions, depth + 1);
    }
    match value {
        serde_json::Value::Object(object) => {
            for nested in object.values_mut() {
                inline_references_in(nested, definitions, depth + 1)?;
            }
        }
        serde_json::Value::Array(values) => {
            for nested in values {
                inline_references_in(nested, definitions, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn validate(
    name: &str,
    schema: &serde_json::Map<String, serde_json::Value>,
    row: &Capability,
    known: &BTreeSet<String>,
) -> Result<(), SurfaceError> {
    if row.tmux_effects.is_empty() {
        return Err(SurfaceError::new(format!(
            "tool {name:?} has no direct tmux effect"
        )));
    }
    let schema_keys: BTreeSet<String> = schema
        .get("properties")
        .and_then(serde_json::Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default();
    let sink_keys: BTreeSet<String> = row.input_sinks.keys().cloned().collect();
    if schema_keys != sink_keys {
        return Err(SurfaceError::new(format!(
            "tool {name:?} input sinks do not equal its schema keys: schema={schema_keys:?}, sinks={sink_keys:?}"
        )));
    }
    for (input, sinks) in &row.input_sinks {
        if sinks.is_empty() {
            return Err(SurfaceError::new(format!(
                "tool {name:?} input {input:?} has no sink"
            )));
        }
        if sinks.len() > 1 && sinks.contains(&InputSink::None) {
            return Err(SurfaceError::new(format!(
                "tool {name:?} input {input:?} combines none with another sink"
            )));
        }
    }
    let format_inputs: BTreeSet<_> = row
        .input_sinks
        .iter()
        .filter(|(_, sinks)| sinks.contains(&InputSink::TmuxFormat))
        .map(|(input, _)| input.clone())
        .collect();
    let controlled_inputs: BTreeSet<_> = row.input_literalization.keys().cloned().collect();
    if format_inputs != controlled_inputs {
        return Err(SurfaceError::new(format!(
            "tool {name:?} tmux-format sinks do not equal input literalization keys"
        )));
    }
    let has_pane_input = row
        .input_sinks
        .values()
        .any(|sinks| sinks.contains(&InputSink::PaneInput));
    let has_pane_command = row
        .input_sinks
        .values()
        .any(|sinks| sinks.contains(&InputSink::ShellCommand));
    let has_nested_tool = row
        .input_sinks
        .values()
        .any(|sinks| sinks.contains(&InputSink::NestedTool));
    match row.process_reach {
        ProcessReach::PaneInput if !has_pane_input => {
            return Err(SurfaceError::new(format!(
                "tool {name:?} declares pane-input reach without a pane-input sink"
            )));
        }
        ProcessReach::PaneCommand if !has_pane_command => {
            return Err(SurfaceError::new(format!(
                "tool {name:?} declares pane-command reach without a pane-command sink"
            )));
        }
        ProcessReach::None | ProcessReach::ConfiguredProcess
            if has_pane_input || has_pane_command =>
        {
            return Err(SurfaceError::new(format!(
                "tool {name:?} input sinks exceed its process reach"
            )));
        }
        _ => {}
    }
    validate_authority(name, row, known, has_nested_tool)?;
    Ok(())
}

fn validate_authority(
    name: &str,
    row: &Capability,
    known: &BTreeSet<String>,
    has_nested_tool: bool,
) -> Result<(), SurfaceError> {
    if !row.nested_authority.is_subset(known) {
        return Err(SurfaceError::new(format!(
            "tool {name:?} names unknown nested authority"
        )));
    }
    if row.nested_authority.contains(name) {
        return Err(SurfaceError::new(format!(
            "tool {name:?} includes itself in nested authority"
        )));
    }
    if has_nested_tool == row.nested_authority.is_empty() {
        return Err(SurfaceError::new(format!(
            "tool {name:?} must declare nested-tool sinks and nested authority together"
        )));
    }
    if row.amplifies_future_input != (name == "set_synchronize_panes") {
        return Err(SurfaceError::new(format!(
            "tool {name:?} has an invalid future-input amplification declaration"
        )));
    }
    Ok(())
}

/// Attach one typed capability row to an SDK-native tool route.
#[macro_export]
macro_rules! capability_meta {
    (
        $toolset:ident, $reach:ident, [$($effect:ident),+ $(,)?],
        [$($output:ident),* $(,)?], $secrets:expr, $untrusted:expr,
        {$($input:literal => [$($sink:ident),+ $(,)?]),* $(,)?}
    ) => {
        $crate::capability_meta!(
            $toolset, $reach,
            effects = [$($effect),+],
            outputs = [$($output),*],
            secrets = $secrets,
            untrusted = $untrusted,
            sinks = {$($input => [$($sink),+]),*},
            literalized = [],
            format_validated = [],
            nested = [],
            self_bounded = false,
            always_load = false,
        )
    };
    (
        $toolset:ident, $reach:ident, [$($effect:ident),+ $(,)?],
        [$($output:ident),* $(,)?], $secrets:expr, $untrusted:expr,
        {$($input:literal => [$($sink:ident),+ $(,)?]),* $(,)?};
        always_load
    ) => {
        $crate::capability_meta!(
            $toolset, $reach,
            effects = [$($effect),+],
            outputs = [$($output),*],
            secrets = $secrets,
            untrusted = $untrusted,
            sinks = {$($input => [$($sink),+]),*},
            literalized = [],
            format_validated = [],
            nested = [],
            self_bounded = false,
            always_load = true,
        )
    };
    (
        $toolset:ident, $reach:ident,
        effects = [$($effect:ident),+ $(,)?],
        outputs = [$($output:ident),* $(,)?],
        secrets = $secrets:expr,
        untrusted = $untrusted:expr,
        sinks = {$($input:literal => [$($sink:ident),+ $(,)?]),* $(,)?},
        literalized = [$($literalized:literal),* $(,)?],
        $(format_validated = [$($validated:literal),* $(,)?],)?
        nested = [$($nested:literal),* $(,)?],
        self_bounded = $self_bounded:expr,
        always_load = $always_load:expr $(,)?
    ) => {
        $crate::capability_meta!(
            $toolset, $reach,
            effects = [$($effect),+],
            outputs = [$($output),*],
            secrets = $secrets,
            untrusted = $untrusted,
            sinks = {$($input => [$($sink),+]),*},
            literalized = [$($literalized),*],
            format_validated = [$($($validated),*)?],
            nested = [$($nested),*],
            amplifies_future_input = false,
            self_bounded = $self_bounded,
            always_load = $always_load,
        )
    };
    (
        $toolset:ident, $reach:ident,
        effects = [$($effect:ident),+ $(,)?],
        outputs = [$($output:ident),* $(,)?],
        secrets = $secrets:expr,
        untrusted = $untrusted:expr,
        sinks = {$($input:literal => [$($sink:ident),+ $(,)?]),* $(,)?},
        literalized = [$($literalized:literal),* $(,)?],
        $(format_validated = [$($validated:literal),* $(,)?],)?
        nested = [$($nested:literal),* $(,)?],
        amplifies_future_input = $amplifies:expr,
        self_bounded = $self_bounded:expr,
        always_load = $always_load:expr $(,)?
    ) => {{
        $crate::manifest::metadata(
            $crate::manifest::Capability {
                toolset: $crate::policy::Toolset::$toolset,
                process_reach: $crate::manifest::ProcessReach::$reach,
                tmux_effects: [$($crate::manifest::TmuxEffect::$effect),+]
                    .into_iter()
                    .collect(),
                output_classes: [$($crate::manifest::OutputClass::$output),*]
                    .into_iter()
                    .collect(),
                may_expose_secrets: $secrets,
                may_return_untrusted_content: $untrusted,
                annotations: $crate::manifest::Annotations::CONSERVATIVE,
                input_sinks: [$(
                    (
                        $input.to_owned(),
                        [$($crate::manifest::InputSink::$sink),+]
                            .into_iter()
                            .collect(),
                    )
                ),*]
                    .into_iter()
                    .collect(),
                input_literalization: [
                    $(
                        (
                            $literalized.to_owned(),
                            $crate::manifest::InputLiteralization::DoubleHashOnce,
                        ),
                    )*
                    $($(
                            (
                                $validated.to_owned(),
                                $crate::manifest::InputLiteralization::ValidatedVariableName,
                            ),
                    )*)?
                ]
                    .into_iter()
                    .collect(),
                nested_authority: [$($nested.to_owned()),*]
                    .into_iter()
                    .collect(),
                amplifies_future_input: $amplifies,
            },
            $always_load,
        )
    }};
}
