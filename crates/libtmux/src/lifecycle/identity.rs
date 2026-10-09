//! An ownership token is accepted on the same connection as daemon identity.

use std::io::Read as _;

use crate::internal::core::Core;
use crate::{Command, CommandChain, Error, ServerGeneration};

pub(crate) const KEY: &str = "@libtmux_owner_generation";
pub(crate) const MARKER: &str = "__libtmux_ownership__ ";
pub(crate) const FORMAT: &str = "#{pid} #{start_time} #{@libtmux_owner_generation}";

#[derive(Clone, Copy, Debug)]
pub(crate) struct DaemonIdentity {
    pub(crate) generation: ServerGeneration,
    token: [u8; 32],
}

impl DaemonIdentity {
    pub(crate) fn token(&self) -> &str {
        // Construction accepts ASCII hexadecimal bytes only.
        std::str::from_utf8(&self.token).unwrap_or_default()
    }
}

pub(crate) fn initialize() -> Result<Command, Error> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut random = [0_u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut random))
        .map_err(|error| Error::OwnershipMetadata {
            reason: format!("cannot read random ownership token: {error}"),
        })?;
    let token: String = random
        .iter()
        .flat_map(|byte| {
            [
                HEX[usize::from(byte >> 4)] as char,
                HEX[usize::from(byte & 15)] as char,
            ]
        })
        .collect();
    // -o preserves even an empty existing option: malformed metadata is an
    // error, never permission to overwrite another owner's token.
    Ok(Command::new("set-option").arg("-soq").arg(KEY).arg(token))
}

pub(crate) fn receipt() -> Command {
    Command::new("display-message")
        .arg("-p")
        .arg(format!("{MARKER}{FORMAT}"))
}

pub(crate) fn parse(bytes: &[u8]) -> Result<DaemonIdentity, Error> {
    let text = std::str::from_utf8(bytes.strip_suffix(b"\n").unwrap_or(bytes)).unwrap_or_default();
    let (generation, token) = text.rsplit_once(' ').ok_or_else(invalid)?;
    if token.len() != 32 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    Ok(DaemonIdentity {
        generation: super::parse_generation(generation.as_bytes())?,
        token: token.as_bytes().try_into().map_err(|_| invalid())?,
    })
}

fn invalid() -> Error {
    Error::OwnershipMetadata {
        reason: format!("{KEY} must contain exactly 32 ASCII hexadecimal characters"),
    }
}

pub(crate) async fn accept(core: &Core) -> Result<DaemonIdentity, Error> {
    let result = core
        .execute_chain_no_start(CommandChain::new(initialize()?).then(receipt()))
        .await?;
    if !result.success() {
        return Err(Error::from_refused_result(
            "ownership-metadata",
            &result,
            None,
        ));
    }
    let answer = result
        .stdout()
        .strip_prefix(MARKER.as_bytes())
        .ok_or_else(invalid)?;
    parse(answer)
}
