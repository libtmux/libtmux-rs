//! Lossless text and byte forms shared by stored operation operands.
use serde::{Deserialize, Serialize};
use std::ffi::{OsStr, OsString};

#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub(crate) enum Argument {
    Text(String),
    Bytes(Vec<u8>),
}

#[cfg_attr(
    unix,
    allow(clippy::unnecessary_wraps, reason = "non-Unix conversions can fail")
)]
impl Argument {
    pub(crate) fn from_os(value: &OsStr) -> Result<Self, &'static str> {
        if let Some(text) = value.to_str() {
            return Ok(Self::Text(text.to_owned()));
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt as _;
            Ok(Self::Bytes(value.as_bytes().to_vec()))
        }
        #[cfg(not(unix))]
        Err("OS argument cannot be represented as tmux bytes")
    }

    pub(crate) fn into_os(self) -> Result<OsString, &'static str> {
        match self {
            Self::Text(text) => Ok(text.into()),
            Self::Bytes(bytes) => {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStringExt as _;
                    Ok(OsString::from_vec(bytes))
                }
                #[cfg(not(unix))]
                String::from_utf8(bytes)
                    .map(OsString::from)
                    .map_err(|_| "tmux bytes cannot be represented as an OS argument")
            }
        }
    }
}
