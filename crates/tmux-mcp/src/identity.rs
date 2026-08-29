//! One identity for state retained by an MCP server instance.

use std::sync::{Mutex, MutexGuard, PoisonError};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(crate) struct InstanceId(u128);

impl InstanceId {
    pub(crate) fn decode(text: &str) -> Option<Self> {
        if text.len() != 32 {
            return None;
        }
        u128::from_str_radix(text, 16).ok().map(Self)
    }
}

impl std::fmt::Debug for InstanceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("InstanceId(..)")
    }
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:032x}", self.0)
    }
}

#[derive(Debug)]
pub(crate) struct InstanceIdentity(Mutex<Option<InstanceId>>);

impl InstanceIdentity {
    pub(crate) const fn new() -> Self {
        Self(Mutex::new(None))
    }

    pub(crate) fn get(&self) -> Result<InstanceId, getrandom::Error> {
        let mut current = hold(&self.0);
        if let Some(identity) = *current {
            return Ok(identity);
        }

        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes)?;
        let identity = InstanceId(u128::from_le_bytes(bytes));
        *current = Some(identity);
        Ok(identity)
    }

    #[cfg(test)]
    pub(crate) const fn fixed(identity: u128) -> Self {
        Self(Mutex::new(Some(InstanceId(identity))))
    }
}

fn hold<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(PoisonError::into_inner)
}
