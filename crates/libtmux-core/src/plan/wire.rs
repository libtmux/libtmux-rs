//! Serializing a plan without lying about what tmux arguments can hold.
//!
//! Two things resist a text wire format. A tmux ID is a struct but reads as
//! `%1`, and writing it as an object would make a hand-written plan tedious.
//! A command argument is operating-system bytes, which need not be UTF-8 at
//! all, so writing it as a string would either corrupt it or refuse it.
//!
//! IDs therefore serialize as their rendered text, and an argument serializes
//! as text when it is text and as an array of bytes when it is not. Both
//! round-trip exactly, and the common case stays readable.

use std::ffi::OsString;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Serialize an ID as the text tmux prints for it.
pub(super) fn id<S, T>(value: &T, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: AsRef<str>,
{
    serializer.serialize_str(value.as_ref())
}

/// Read an ID back from that text.
pub(super) fn parse_id<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    let text = String::deserialize(deserializer)?;
    text.parse().map_err(D::Error::custom)
}

pub(super) use crate::wire_arg::Argument;

#[cfg(feature = "schema")]
fn id_schema(generator: &mut schemars::SchemaGenerator, sigil: &str) -> schemars::Schema {
    use schemars::JsonSchema as _;

    const U32: &str = concat!(
        "0*(?:[0-9]{1,9}|[0-3][0-9]{9}|4[0-1][0-9]{8}|42[0-8][0-9]{7}|",
        "429[0-3][0-9]{6}|4294[0-8][0-9]{5}|42949[0-5][0-9]{4}|",
        "429496[0-6][0-9]{3}|4294967[0-1][0-9]{2}|42949672[0-8][0-9]|",
        "429496729[0-5])"
    );
    let mut schema = String::json_schema(generator);
    schema.insert("pattern".into(), format!("^{sigil}{U32}$").into());
    schema
}

#[cfg(feature = "schema")]
pub(super) fn session_id_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    id_schema(generator, r"\$")
}

#[cfg(feature = "schema")]
pub(super) fn window_id_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    id_schema(generator, "@")
}

#[cfg(feature = "schema")]
pub(super) fn pane_id_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
    id_schema(generator, "%")
}

/// Serialize one argument.
pub(super) fn argument<S: Serializer>(value: &OsString, serializer: S) -> Result<S::Ok, S::Error> {
    Argument::from_os(value)
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

/// Read one argument back.
pub(super) fn parse_argument<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<OsString, D::Error> {
    Argument::deserialize(deserializer)?
        .into_os()
        .map_err(D::Error::custom)
}

/// The same, for an argument a command may omit.
///
/// Takes `&Option<_>` rather than `Option<&_>` because that is the shape
/// serde's `serialize_with` hands a field of this type.
#[allow(
    clippy::ref_option,
    reason = "the signature serde's serialize_with requires"
)]
pub(super) fn optional_argument<S: Serializer>(
    value: &Option<OsString>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match value {
        Some(value) => argument(value, serializer),
        None => serializer.serialize_none(),
    }
}

/// Read back an argument a command may omit.
pub(super) fn parse_optional_argument<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<OsString>, D::Error> {
    Option::<Argument>::deserialize(deserializer)?
        .map(Argument::into_os)
        .transpose()
        .map_err(D::Error::custom)
}

/// The same, for the name/value pairs an operation carries.
pub(super) fn pairs<S: Serializer>(
    value: &[(OsString, OsString)],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value
        .iter()
        .map(|(name, item)| Ok((Argument::from_os(name)?, Argument::from_os(item)?)))
        .collect::<Result<Vec<_>, &'static str>>()
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

/// Read those pairs back.
pub(super) fn parse_pairs<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<(OsString, OsString)>, D::Error> {
    Vec::<(Argument, Argument)>::deserialize(deserializer)?
        .into_iter()
        .map(|(name, value)| Ok((name.into_os()?, value.into_os()?)))
        .collect::<Result<Vec<_>, &'static str>>()
        .map_err(D::Error::custom)
}

/// The same, for a list of key names an operation sends.
pub(super) fn list<S: Serializer>(value: &[OsString], serializer: S) -> Result<S::Ok, S::Error> {
    value
        .iter()
        .map(|value| Argument::from_os(value))
        .collect::<Result<Vec<_>, _>>()
        .map_err(serde::ser::Error::custom)?
        .serialize(serializer)
}

/// Read that list back.
pub(super) fn parse_list<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<OsString>, D::Error> {
    Vec::<Argument>::deserialize(deserializer)?
        .into_iter()
        .map(Argument::into_os)
        .collect::<Result<Vec<_>, _>>()
        .map_err(D::Error::custom)
}

/// Serialize a duration as seconds, the unit tmuxp and `run-shell -d` use.
pub(super) fn seconds<S: Serializer>(
    value: &std::time::Duration,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_f64(value.as_secs_f64())
}

/// Read seconds back, refusing a negative, infinite or oversized number.
pub(super) fn parse_seconds<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<std::time::Duration, D::Error> {
    let seconds = f64::deserialize(deserializer)?;
    std::time::Duration::try_from_secs_f64(seconds).map_err(D::Error::custom)
}
