//! Portable plan representation of a detached session request.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{NewSession, TmuxArg};

use crate::wire_arg::Argument;

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct Format {
    format: Argument,
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
enum Name {
    Literal(Argument),
    Expression(Format),
}

impl Name {
    fn from_arg(arg: &TmuxArg) -> Result<Self, &'static str> {
        match arg.literal_value() {
            Some(text) => Ok(Self::Literal(Argument::from_os(text)?)),
            None => Ok(Self::Expression(Format {
                format: Argument::from_os(arg.as_os_str())?,
            })),
        }
    }

    fn into_arg(self) -> Result<TmuxArg, &'static str> {
        match self {
            Self::Literal(value) => Ok(TmuxArg::literal(value.into_os()?)),
            Self::Expression(expression) => Ok(TmuxArg::format(expression.format.into_os()?)),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct SessionRequest {
    name: Name,
    start_directory: Option<Name>,
    window_name: Option<Name>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    command: Option<Argument>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    environment: Vec<(Argument, Argument)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<(u32, u32)>,
}

impl Serialize for NewSession {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        fn encode(request: &NewSession) -> Result<SessionRequest, &'static str> {
            Ok(SessionRequest {
                name: Name::from_arg(&request.name)?,
                start_directory: request
                    .start_directory
                    .as_ref()
                    .map(Name::from_arg)
                    .transpose()?,
                window_name: request
                    .window_name
                    .as_ref()
                    .map(Name::from_arg)
                    .transpose()?,
                command: request
                    .command
                    .as_deref()
                    .map(Argument::from_os)
                    .transpose()?,
                environment: request
                    .environment
                    .iter()
                    .map(|(key, value)| Ok((Argument::from_os(key)?, Argument::from_os(value)?)))
                    .collect::<Result<_, &'static str>>()?,
                size: request.width.zip(request.height),
            })
        }
        encode(self)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NewSession {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        fn decode(request: SessionRequest) -> Result<NewSession, &'static str> {
            let (width, height) = request.size.unzip();
            Ok(NewSession {
                name: request.name.into_arg()?,
                start_directory: request.start_directory.map(Name::into_arg).transpose()?,
                window_name: request.window_name.map(Name::into_arg).transpose()?,
                command: request.command.map(Argument::into_os).transpose()?,
                environment: request
                    .environment
                    .into_iter()
                    .map(|(key, value)| Ok((key.into_os()?, value.into_os()?)))
                    .collect::<Result<_, &'static str>>()?,
                width,
                height,
            })
        }
        decode(SessionRequest::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "schema")]
impl schemars::JsonSchema for NewSession {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "NewSession".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        SessionRequest::json_schema(generator)
    }
}
