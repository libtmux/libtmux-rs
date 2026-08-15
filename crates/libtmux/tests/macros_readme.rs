//! The example on `libtmux-macros`'s front page, compiled.
//!
//! A proc-macro crate cannot doctest a README that uses the crate deriving
//! through it, so the example would otherwise be untested prose on the page a
//! reader lands on first.
#![cfg(feature = "query")]
#![cfg(feature = "derive")]

use libtmux::query::{Filterable as _, QueryIteratorExt as _};

#[derive(libtmux::Filterable)]
#[filterable(target = "job", crate = "libtmux")]
struct Job {
    name: String,
    attempts: u32,
}

#[test]
fn the_macros_readme_example_still_works() {
    let jobs = [
        Job {
            name: "build".into(),
            attempts: 3,
        },
        Job {
            name: "deploy".into(),
            attempts: 1,
        },
    ];

    let fields = Job::filter_fields();
    let retried = fields.name.starts_with("bui").and(fields.attempts.gt(2));

    assert_eq!(jobs.iter().matching(&retried).count(), 1);
}
