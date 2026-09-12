//! Closed JSON Schema vocabularies for public string fields.

#![allow(
    dead_code,
    reason = "schemars reads these types through field attributes"
)]

use libtmux::{ResizeDirection, SplitDirection};
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

/// Every [`SplitDirection`] this server advertises, and its wire word.
///
/// The proxy enums above are what a client sees in `tools/list`, and until now
/// nothing tied them to the libtmux enums they stand for. A variant added
/// there would have gone unadvertised and unreachable with every gate green,
/// because the handlers matched on strings and so were exhaustive over the
/// words rather than over the directions.
///
/// These matches are exhaustive over the libtmux enum, so a new variant fails
/// this build instead. The table is also what the handlers parse with, so the
/// word a client may send and the word this crate accepts cannot disagree, and
/// `mcp_vocabulary_matches_the_core_enums` checks the words against the proxy a
/// client is actually shown.
///
/// What this does not catch on its own is a variant missing from the arrays
/// below: the compiler checks that every direction has a word, not that every
/// direction is listed. The failing match is where a reader is sent to fix it.
pub(crate) const fn split_direction_word(direction: SplitDirection) -> &'static str {
    match direction {
        SplitDirection::Above => "above",
        SplitDirection::Below => "below",
        SplitDirection::Left => "left",
        SplitDirection::Right => "right",
    }
}

/// The same for [`ResizeDirection`].
pub(crate) const fn resize_direction_word(direction: ResizeDirection) -> &'static str {
    match direction {
        ResizeDirection::Up => "up",
        ResizeDirection::Down => "down",
        ResizeDirection::Left => "left",
        ResizeDirection::Right => "right",
    }
}

pub(crate) const SPLIT_DIRECTIONS: [SplitDirection; 4] = [
    SplitDirection::Above,
    SplitDirection::Below,
    SplitDirection::Left,
    SplitDirection::Right,
];

pub(crate) const RESIZE_DIRECTIONS: [ResizeDirection; 4] = [
    ResizeDirection::Up,
    ResizeDirection::Down,
    ResizeDirection::Left,
    ResizeDirection::Right,
];

/// Resolve a wire word to the direction it names.
pub(crate) fn split_direction(word: &str) -> Option<SplitDirection> {
    SPLIT_DIRECTIONS
        .into_iter()
        .find(|direction| split_direction_word(*direction) == word)
}

/// The same for a resize.
pub(crate) fn resize_direction(word: &str) -> Option<ResizeDirection> {
    RESIZE_DIRECTIONS
        .into_iter()
        .find(|direction| resize_direction_word(*direction) == word)
}

/// The words this server accepts for a direction, in advertised order.
pub(crate) fn split_direction_words() -> Vec<&'static str> {
    SPLIT_DIRECTIONS
        .into_iter()
        .map(split_direction_word)
        .collect()
}

/// The same for a resize.
pub(crate) fn resize_direction_words() -> Vec<&'static str> {
    RESIZE_DIRECTIONS
        .into_iter()
        .map(resize_direction_word)
        .collect()
}

/// Render a vocabulary as the prose a refusal names it in.
///
/// Spelled the way the directions this server already refuses are spelled --
/// "up, down, left, or right" -- so a generated list and a written one read
/// the same to whoever receives them.
pub(crate) fn words_or(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [only] => (*only).to_owned(),
        [first, second] => format!("{first} or {second}"),
        [rest @ .., last] => format!("{}, or {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ResizeDirectionSchema, SplitDirectionSchema, resize_direction, resize_direction_words,
        split_direction, split_direction_words,
    };

    /// Read the words a proxy enum advertises to a client.
    fn advertised<T: schemars::JsonSchema>() -> Vec<String> {
        let schema = serde_json::to_value(schemars::schema_for!(T)).expect("a schema");
        schema["enum"]
            .as_array()
            .expect("a closed vocabulary is a JSON Schema enum")
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .expect("a vocabulary word is a string")
                    .to_owned()
            })
            .collect()
    }

    /// The words clients are shown are the words the libtmux enums produce.
    ///
    /// The proxies exist so a libtmux change cannot silently move this
    /// server's advertised protocol, which means they can go stale instead.
    /// This is the half the compiler cannot check: the exhaustive matches in
    /// this module prove every direction has a word, and this proves every
    /// word is offered.
    #[test]
    fn mcp_vocabulary_matches_the_core_enums() {
        assert_eq!(
            advertised::<SplitDirectionSchema>(),
            split_direction_words()
        );
        assert_eq!(
            advertised::<ResizeDirectionSchema>(),
            resize_direction_words()
        );
    }

    /// Every advertised word parses back, and nothing else does.
    #[test]
    fn every_advertised_direction_resolves() {
        for word in split_direction_words() {
            assert!(split_direction(word).is_some(), "split {word} resolves");
        }
        for word in resize_direction_words() {
            assert!(resize_direction(word).is_some(), "resize {word} resolves");
        }
        assert!(split_direction("sideways").is_none());
        assert!(resize_direction("above").is_none(), "not a resize word");
    }

    /// A generated vocabulary reads like the hand-written refusals beside it.
    #[test]
    fn a_vocabulary_reads_as_prose() {
        use super::words_or;

        assert_eq!(
            words_or(&split_direction_words()),
            "above, below, left, or right"
        );
        assert_eq!(
            words_or(&resize_direction_words()),
            "up, down, left, or right"
        );
        assert_eq!(words_or(&["next", "previous"]), "next or previous");
        assert_eq!(words_or(&["only"]), "only");
        assert_eq!(words_or(&[]), "");
    }
}
