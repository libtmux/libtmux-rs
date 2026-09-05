//! One typed capability definition bound to each native MCP tool route.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{MetaObject, ToolAnnotations};
use serde::{Deserialize, Serialize};

use crate::TmuxTools;
use crate::policy::{Selection, SurfaceError, Toolset};

pub(crate) const CAPABILITY_KEY: &str = "com.git-pull.libtmux-mcp/capability";

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
    TmuxArgument,
    TmuxFormat,
    PaneInput,
    PaneCommand,
    HostCommand,
    RegularExpression,
    FilterExpression,
    FilesystemPath,
    NestedTool,
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
pub(crate) struct Capability {
    pub(crate) toolset: Toolset,
    pub(crate) process_reach: ProcessReach,
    pub(crate) tmux_effects: BTreeSet<TmuxEffect>,
    pub(crate) output_classes: BTreeSet<OutputClass>,
    pub(crate) may_expose_secrets: bool,
    pub(crate) may_return_untrusted_content: bool,
    pub(crate) annotations: Annotations,
    pub(crate) input_sinks: BTreeMap<String, BTreeSet<InputSink>>,
    pub(crate) literalized_tmux_formats: BTreeSet<String>,
    pub(crate) validated_tmux_formats: BTreeSet<String>,
    pub(crate) nested_authority: BTreeSet<String>,
    pub(crate) self_bounded: bool,
}

impl Capability {
    pub(crate) fn controlled_opener(&self) -> &'static str {
        match self.process_reach {
            ProcessReach::ConfiguredProcess => {
                "Start a pane's configured process; accepts no command payload."
            }
            ProcessReach::PaneInput => {
                "Send input to a pane's program; a shell that receives it runs it with your user's permissions."
            }
            ProcessReach::PaneCommand => {
                "Run a shell command in a pane with your user's permissions."
            }
            ProcessReach::None => match self.toolset {
                Toolset::Manage | Toolset::Execute => {
                    "Change tmux state; no client-supplied executable input."
                }
                Toolset::Teardown => "Delete tmux state; accepts no command payload.",
                Toolset::Inspect if self.output_classes.contains(&OutputClass::TerminalContent) => {
                    "Read pane output; accepts no client-supplied executable input. Returned content may be sensitive or untrusted."
                }
                Toolset::Inspect
                    if self
                        .output_classes
                        .contains(&OutputClass::ProcessEnvironment) =>
                {
                    "Read the tmux environment; accepts no client-supplied executable input. Returned values may contain secrets."
                }
                Toolset::Inspect
                    if self
                        .output_classes
                        .contains(&OutputClass::ConfiguredCommand) =>
                {
                    "Read configured tmux commands; accepts no client-supplied executable input. Returned values may contain executable configuration."
                }
                Toolset::Inspect => {
                    "Inspect tmux metadata; accepts no client-supplied executable input."
                }
            },
        }
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
        CAPABILITY_KEY.to_owned(),
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
        .and_then(|meta| meta.0.get(CAPABILITY_KEY))
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
    pub(crate) title: Option<String>,
    #[serde(flatten)]
    pub(crate) capability: Capability,
}

#[cfg(test)]
impl ReportTool {
    pub(crate) fn controlled_opener(&self) -> &'static str {
        self.capability.controlled_opener()
    }
}

/// The frozen, effective MCP surface reported at `tmux://capabilities`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityReport {
    pub(crate) contract_version: u8,
    pub(crate) frozen: bool,
    pub(crate) selected_toolsets: Vec<&'static str>,
    pub(crate) included_tools: Vec<String>,
    pub(crate) excluded_tools: Vec<String>,
    pub(crate) selected_socket: String,
    pub(crate) socket_provenance: crate::policy::SocketProvenance,
    pub(crate) minimal_config_provenance: bool,
    pub(crate) tools: Vec<ReportTool>,
}

pub(crate) struct Resolved {
    pub(crate) router: ToolRouter<TmuxTools>,
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
        capabilities.insert(name.to_string(), row);
    }

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

    let effective: BTreeSet<_> = capabilities.keys().cloned().collect();
    for row in capabilities.values_mut() {
        row.nested_authority.retain(|name| effective.contains(name));
    }

    let tools = capabilities
        .into_iter()
        .map(|(name, capability)| ReportTool {
            title: router.get(&name).and_then(|tool| tool.title.clone()),
            name,
            capability,
        })
        .collect();
    Ok(Resolved {
        router,
        report: CapabilityReport {
            contract_version: 1,
            frozen: true,
            selected_toolsets: selection
                .toolsets()
                .iter()
                .map(|toolset| toolset.name())
                .collect(),
            included_tools: selection.included_names().iter().cloned().collect(),
            excluded_tools: selection.excluded_names().iter().cloned().collect(),
            selected_socket: String::new(),
            socket_provenance: crate::policy::SocketProvenance::Unknown,
            minimal_config_provenance: false,
            tools,
        },
    })
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
        if sinks.contains(&InputSink::HostCommand) {
            return Err(SurfaceError::new(format!(
                "tool {name:?} exposes prohibited host-command input {input:?}"
            )));
        }
        if sinks.contains(&InputSink::TmuxFormat)
            && !row.literalized_tmux_formats.contains(input)
            && !row.validated_tmux_formats.contains(input)
        {
            return Err(SurfaceError::new(format!(
                "tool {name:?} leaves tmux-format input {input:?} unrestricted"
            )));
        }
    }
    let has_pane_input = row
        .input_sinks
        .values()
        .any(|sinks| sinks.contains(&InputSink::PaneInput));
    let has_pane_command = row
        .input_sinks
        .values()
        .any(|sinks| sinks.contains(&InputSink::PaneCommand));
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
    if !row.literalized_tmux_formats.is_subset(&schema_keys) {
        return Err(SurfaceError::new(format!(
            "tool {name:?} literalizes an input outside its schema"
        )));
    }
    if !row.validated_tmux_formats.is_subset(&schema_keys) {
        return Err(SurfaceError::new(format!(
            "tool {name:?} validates an input outside its schema"
        )));
    }
    if !row.nested_authority.is_subset(known) {
        return Err(SurfaceError::new(format!(
            "tool {name:?} names unknown nested authority"
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
            validated = [],
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
            validated = [],
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
        validated = [$($validated:literal),* $(,)?],
        nested = [$($nested:literal),* $(,)?],
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
                literalized_tmux_formats: [$($literalized.to_owned()),*]
                    .into_iter()
                    .collect(),
                validated_tmux_formats: [$($validated.to_owned()),*]
                    .into_iter()
                    .collect(),
                nested_authority: [$($nested.to_owned()),*]
                    .into_iter()
                    .collect(),
                self_bounded: $self_bounded,
            },
            $always_load,
        )
    }};
}
