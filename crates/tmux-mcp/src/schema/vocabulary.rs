//! Closed JSON Schema vocabularies for public string fields.

#![allow(
    dead_code,
    reason = "schemars reads these types through field attributes"
)]

use std::borrow::Cow;

use libtmux::plan::OperationKind;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};

#[derive(JsonSchema)]
#[schemars(rename_all = "snake_case")]
pub(crate) enum PlanGroupingSchema {
    Sequential,
    Folding,
    Marked,
}

#[derive(JsonSchema)]
#[schemars(rename_all = "snake_case")]
pub(crate) enum SplitDirectionSchema {
    Above,
    Below,
    Left,
    Right,
}

#[derive(JsonSchema)]
#[schemars(rename_all = "snake_case")]
pub(crate) enum ResizeDirectionSchema {
    Up,
    Down,
    Left,
    Right,
}

#[derive(JsonSchema)]
#[schemars(rename_all = "snake_case")]
pub(crate) enum SelectPaneDirectionSchema {
    Up,
    Down,
    Left,
    Right,
    Last,
    Next,
    Previous,
}

#[derive(JsonSchema)]
#[schemars(rename_all = "snake_case")]
pub(crate) enum SelectWindowDirectionSchema {
    Next,
    Previous,
    Last,
}

#[derive(JsonSchema)]
#[schemars(rename_all = "kebab-case")]
pub(crate) enum OptionScopeSchema {
    Server,
    GlobalSession,
    GlobalWindow,
    Session,
    Window,
    Pane,
}

/// The operations a plan report can name, read from the core's own set.
///
/// Restating the variants here would let the two lists disagree the moment
/// `libtmux` gains an operation, and the field carrying them is a `String`,
/// so nothing would fail to compile. The published schema would simply stop
/// admitting a value the server had started sending.
pub(crate) struct PlanOperationKindSchema;

impl JsonSchema for PlanOperationKindSchema {
    fn schema_name() -> Cow<'static, str> {
        "PlanOperationKindSchema".into()
    }

    fn schema_id() -> Cow<'static, str> {
        "tmux_mcp::schema::PlanOperationKindSchema".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        let names: Vec<&str> = OperationKind::ALL.iter().map(|kind| kind.name()).collect();
        json_schema!({ "type": "string", "enum": names })
    }
}

#[derive(JsonSchema)]
#[schemars(rename_all = "snake_case")]
pub(crate) enum PlanOutcomeSchema {
    Complete,
    Failed,
    Skipped,
    Unknown,
}

#[derive(JsonSchema)]
#[schemars(rename_all = "snake_case")]
pub(crate) enum PlanAttributionSchema {
    PerCommand,
    Merged,
}

#[derive(JsonSchema)]
pub(crate) enum WatchStopSchema {
    #[schemars(rename = "deadline")]
    Deadline,
    #[schemars(rename = "pane closed")]
    PaneClosed,
    #[schemars(rename = "byte limit")]
    ByteLimit,
}

#[derive(JsonSchema)]
#[schemars(rename_all = "snake_case")]
pub(crate) enum ChannelWaitOutcomeSchema {
    Signalled,
    Deadline,
}
