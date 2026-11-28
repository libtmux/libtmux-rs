use std::ffi::OsStr;

use libtmux_core::control::{ControlCommandRefusal, require_shared_control};

use crate::Error;

pub(super) use libtmux_core::control::valid_subscription_name;

pub(crate) fn require_shared(command: &[impl AsRef<OsStr>]) -> Result<(), Error> {
    require_shared_control(command).map_err(|refusal| match refusal {
        ControlCommandRefusal::BlockingCommand => Error::control_mode_blocking(),
        ControlCommandRefusal::InvalidSubscriptionName => {
            Error::control_mode_invalid_subscription()
        }
        _ => Error::control_mode_unrepresentable(),
    })
}
