//! Closed JSON Schema vocabularies for public string fields.

#![allow(
    dead_code,
    reason = "schemars reads these types through field attributes"
)]

use schemars::JsonSchema;

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

#[derive(JsonSchema)]
#[schemars(rename_all = "snake_case")]
pub(crate) enum ChannelWaitOutcomeSchema {
    Signalled,
    Deadline,
}
