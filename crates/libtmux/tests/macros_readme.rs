//! The examples on `libtmux-macros`'s front page, compiled.
//!
//! A proc-macro crate cannot doctest a README that uses the crate deriving
//! through it, so the examples would otherwise be untested prose on the page a
//! reader lands on first.
#![cfg(feature = "query")]
#![cfg(feature = "derive")]

use libtmux::query::{Filterable as _, QueryIteratorExt as _};

#[derive(libtmux::Filterable)]
#[filterable(target = "job")]
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

#[derive(libtmux::Filterable)]
#[filterable(target = "step")]
struct Step {
    name: String,
}

#[derive(libtmux::Filterable)]
#[filterable(target = "pipeline")]
struct Pipeline {
    name: String,
    #[filterable(many)]
    steps: Vec<Step>,
}

#[test]
fn the_macros_readme_relation_example_still_works() {
    let pipelines = [
        Pipeline {
            name: "ci".into(),
            steps: vec![Step {
                name: "build".into(),
            }],
        },
        Pipeline {
            name: "idle".into(),
            steps: vec![],
        },
    ];

    let pipeline = Pipeline::filter_fields();
    let step = Step::filter_fields();
    let building = pipeline.steps.any(step.name.eq("build"));

    let names: Vec<_> = pipelines
        .iter()
        .matching(&building)
        .map(|pipeline| pipeline.name.as_str())
        .collect();
    assert_eq!(names, ["ci"]);
}
