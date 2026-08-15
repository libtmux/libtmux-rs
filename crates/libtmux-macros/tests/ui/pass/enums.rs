#![allow(dead_code)]

use libtmux_macros::Filterable;
use renamed_libtmux::query::{EnumField, FilterEnum, Filterable as _};

enum State {
    Ready,
    Blocked,
}

impl FilterEnum for State {
    const FILTER_VARIANTS: &'static [&'static str] = &["ready", "blocked"];

    fn filter_name(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Filterable)]
#[filterable(target = "job")]
#[filterable(fields = "JobHandles")]
struct Job {
    #[filterable(rename = "phase", enum)]
    state: State,
    #[filterable(rename = "fallback_state")]
    #[filterable(enum)]
    fallback: Option<State>,
}

fn enum_field(_: EnumField<Job, State>) {}

fn main() {
    let fields: JobHandles = Job::filter_fields();
    enum_field(fields.state);
    enum_field(fields.fallback);
    let present = Job {
        state: State::Ready,
        fallback: Some(State::Blocked),
    };
    let absent = Job {
        state: State::Ready,
        fallback: None,
    };
    assert!(fields.state.eq(State::Ready).matches(&present));
    assert!(fields.fallback.eq(State::Blocked).matches(&present));
    assert!(!fields.fallback.eq(State::Blocked).matches(&absent));
    assert!(fields.fallback.not_in([]).matches(&present));
    assert!(!fields.fallback.not_in([]).matches(&absent));
    assert!(fields.fallback.not_in([]).not().matches(&absent));
    assert!(format!("{:?}", fields.state).contains("phase"));
    assert!(format!("{:?}", fields.fallback).contains("fallback_state"));
}
