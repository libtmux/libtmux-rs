<!-- libtmux-logo -->
<div align="center">
  <picture>
    <source srcset="https://raw.githubusercontent.com/libtmux/libtmux-rs/HEAD/crates/libtmux-macros/assets/logo.svg" type="image/svg+xml">
    <img src="https://raw.githubusercontent.com/libtmux/libtmux-rs/HEAD/crates/libtmux-macros/assets/logo.png" width="128" height="128" alt="libtmux for Rust">
  </picture>
</div>
<!-- /libtmux-logo -->

<div align="center">

# libtmux-macros

[`#[derive(Filterable)]`] for [`libtmux`](https://docs.rs/libtmux): give your
own structs the same [typed query grammar] `libtmux` uses for tmux objects.

</div>

Derive it on a struct, then [filter a list of that struct](#quick-start) with
the expressions `libtmux` uses for panes. A comparison that has no meaning for
a field does not compile.

> Releases are numbered `0.1.N`, and each release increments `N`. The API is
> not settled: any release may change or remove exported identifiers, and the
> changelog marks each break. Pin an exact version to avoid one.

**Contents** — [Quick start](#quick-start) · [Relations](#relations) ·
[Attributes](#attributes) · [Packages](#packages) ·
[What it leaves out](#what-it-leaves-out) ·
[Support and status](#support-and-status) · [Documentation](#documentation)

## Quick start

You do not depend on this crate. `libtmux` re-exports the macro behind its
`derive` feature:

```console
$ cargo add libtmux@0.1.0-alpha.17 --features derive
```

<details>
<summary>Cargo.toml</summary>

```toml
[dependencies]
libtmux = { version = "0.1.0-alpha.17", features = ["derive"] }
```

</details>

### [`#[derive(Filterable)]`]: filter your own structs

Derive it, build an expression from the field handles, and match a list with
it:

```rust
use libtmux::query::{Filterable as _, QueryIteratorExt as _};

#[derive(libtmux::Filterable)]
#[filterable(target = "job")]
struct Job {
    name: String,
    attempts: u32,
}

let jobs = [
    Job { name: "build".into(), attempts: 3 },
    Job { name: "deploy".into(), attempts: 1 },
];

let fields = Job::filter_fields();
let retried = fields.name.starts_with("bui").and(fields.attempts.gt(2));

assert_eq!(jobs.iter().matching(&retried).count(), 1);
```

Field types are read from the struct, and they decide which operations exist:
`attempts.gt(2)` compiles because the field is an integer, and `name.gt(..)`
does not, because text has no ordering. Getting that wrong is a compile error
that names the field.

## Relations

### [`#[filterable(many)]`][derive-docs]: ask what a value contains

A relation lets one expression ask about what a value *contains*. Mark a
`Vec<Child>` field with `many`, and its handle takes the child's field
handles:

```rust
use libtmux::query::{Filterable as _, QueryIteratorExt as _};

#[derive(libtmux::Filterable)]
#[filterable(target = "step")]
struct Step {
    name: String,
}

#[derive(libtmux::Filterable)]
#[filterable(target = "job")]
struct Job {
    name: String,
    #[filterable(many)]
    steps: Vec<Step>,
}

let jobs = [
    Job { name: "ci".into(), steps: vec![Step { name: "build".into() }] },
    Job { name: "idle".into(), steps: vec![] },
];

let job = Job::filter_fields();
let step = Step::filter_fields();
let building = job.steps.any(step.name.eq("build"));

assert_eq!(jobs.iter().matching(&building).count(), 1);
```

`#[filterable(one)]` does the same for an `Option<Child>` field. Both read the
related type's own field handles, so a relation is still one typed expression.

## Attributes

| Attribute | On | What it does |
| --- | --- | --- |
| `#[filterable(target = "…")]` | struct | Names the type in the portable JSON form |
| `#[filterable(rename = "…")]` | field | Uses a different name in expressions |
| `#[filterable(skip)]` | field | Leaves the field out |
| `#[filterable(enum)]` | field | Treats the field as a closed set of values |
| `#[filterable(many)]` | `Vec<T>` field | Declares a one-to-many relation |
| `#[filterable(one)]` | `Option<T>` field | Declares a one-to-one relation |

[`Filterable`][derive-docs] documents each of them. With `libtmux`'s `serde`
feature, the expression they build lowers to the versioned wire format that
[`libtmux::query`][query-docs] describes.

## Packages

### [`libtmux-macros`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux-macros): the derive

This crate: [`#[derive(Filterable)]`] and the attributes above, with no
dependency on `libtmux`.

### [`libtmux`](https://github.com/libtmux/libtmux-rs/tree/master/crates/libtmux): the grammar

Defines the [`Filterable`][query-reference] trait the derive implements, and
re-exports the derive behind its `derive` feature. [Its guide][libtmux-guide]
filters tmux objects with the same expressions.

## What it leaves out

- **Anything but a struct with named fields.** Enums, tuple structs, unit
  structs, and unions are refused, and so is a type alias.
- **Other collections for a relation.** `many` takes exactly `Vec<Related>`,
  and `one` takes exactly `Option<Related>`.
- **A required dependency in `libtmux`.** `libtmux` never requires proc macros:
  its own `Filterable` implementations are hand-written. Turning the feature
  off removes this crate from the build entirely.

## Support and status

- **Versions.** Releases are numbered `0.1.N`, and any release may change the
  API. Pin an exact version.
- **One version with `libtmux`.** The derive expands to paths into a hidden
  core surface, so `libtmux` pins this crate to its own exact version.
- **No tmux needed.** The derive is compile-time code, and the examples above
  filter plain values.

## Documentation

- [`Filterable`][derive-docs] — the derive and the attributes it accepts
- [`libtmux::query`](https://docs.rs/libtmux/latest/libtmux/query/) — the
  expression grammar, relations, and the versioned wire format
- [`Filterable` in the libtmux.org reference][query-reference] — the trait the
  derive implements
- [The `libtmux` guide](https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/README.md#filtering)
  — filtering tmux objects, which uses the same grammar

[derive-docs]: https://docs.rs/libtmux-macros/latest/libtmux_macros/derive.Filterable.html
[query-docs]: https://docs.rs/libtmux/latest/libtmux/query/
[query-reference]: https://libtmux.org/en/rs/latest/reference/query-filterable/
[libtmux-guide]: https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/README.md#filtering

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option.

[`#[derive(Filterable)]`]:
https://libtmux.org/en/rs/latest/reference/query-filterable/
[typed query grammar]: https://libtmux.org/en/rs/latest/concepts/queries/
