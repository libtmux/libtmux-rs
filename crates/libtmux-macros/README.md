# libtmux-macros

Procedural macros for [`libtmux`](https://docs.rs/libtmux). It currently
provides one derive, `Filterable`.

> **Alpha.** The API changes between releases, including in ways that will not
> be called out as breaking, because nothing here is stable yet. Cargo will not
> resolve a prerelease unless the requirement names one, so a plain `0.1`
> requirement does not pick this up: depend on the exact version below, and
> expect to edit it.

You do not need to depend on this crate. `libtmux` re-exports the macro behind
its `derive` feature:

```toml
[dependencies]
libtmux = { version = "0.1.0-alpha.3", features = ["derive"] }
```

`#[derive(Filterable)]` generates typed field handles for your own struct, so
it can be filtered with the same expressions and the same portable JSON
grammar `libtmux` uses for tmux objects:

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
does not, because text has no ordering. Field attributes cover the rest --
`rename`, `skip`, `enum`, and the `many` and `one` relations.

See the [`libtmux::query`](https://docs.rs/libtmux/latest/libtmux/query/)
documentation for the expression grammar, relations, and the versioned wire
format.

## License

MIT, the same as `libtmux`.
