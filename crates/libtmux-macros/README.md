# libtmux-macros

`#[derive(Filterable)]` for [`libtmux`](https://docs.rs/libtmux): give your own
structs the same typed query grammar `libtmux` uses for tmux objects.

> **Alpha.** The API changes between releases, including in ways that will not
> be called out as breaking, because nothing here is stable yet. Cargo will not
> resolve a prerelease unless the requirement names one, so a plain `0.1`
> requirement does not pick this up: depend on the exact version below, and
> expect to edit it.

## Install

You do not depend on this crate. `libtmux` re-exports the macro behind its
`derive` feature:

```console
$ cargo add libtmux@0.1.0-alpha.4 --features derive
```

<details>
<summary>Cargo.toml</summary>

```toml
[dependencies]
libtmux = { version = "0.1.0-alpha.4", features = ["derive"] }
```

</details>

`libtmux` itself never requires proc macros: its own `Filterable`
implementations are hand-written, and this derive exists for structs outside
that crate. Turning the feature off removes the dependency entirely.

## Use it

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
naming the field, not an expression that quietly matches nothing.

## Attributes

| Attribute | On | What it does |
| --- | --- | --- |
| `#[filterable(target = "…")]` | struct | Names the type in the portable JSON form |
| `#[filterable(rename = "…")]` | field | Uses a different name in expressions |
| `#[filterable(skip)]` | field | Leaves the field out |
| `#[filterable(enum)]` | field | Treats the field as a closed set of values |
| `#[filterable(many = …)]` | field | Declares a one-to-many relation |
| `#[filterable(one = …)]` | field | Declares a one-to-one relation |

A relation lets a question about what a value *contains* stay one expression:
`parents.children.any(children.name.eq("build"))`.

## Documentation

- [`libtmux::query`](https://docs.rs/libtmux/latest/libtmux/query/) — the
  expression grammar, relations, and the versioned wire format
- [The `libtmux` guide](https://github.com/libtmux/libtmux-rs/blob/master/crates/libtmux/README.md#filtering)
  — filtering tmux objects, which uses the same grammar

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE)
or [MIT license](LICENSE-MIT) at your option.
