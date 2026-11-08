# Expected structure of tmuxp's examples

Each `<name>.json` here is the structure tmuxp 1.74.0 builds from
`../<name>.yaml`, written by tmuxp's own loader and not by hand. A port that
loads the same file can map its model to
[`expected.schema.json`](expected.schema.json) and compare, which checks more
than that a file loads or is refused. This directory is the one copy; a port
copies it beside its own catalog and keeps the provenance below.

## How they were generated

```console
$ uv run --no-project --python 3.13 --with tmuxp==1.74.0 generate.py
```

| Input | Value |
| --- | --- |
| tmuxp | 1.74.0 (`v1.74.0`; the 23 examples in `..` are byte-identical to `examples/*.yaml` and `LICENSE` at that tag) |
| Python | 3.13 |
| Steps | `ConfigReader._from_file`, `loader.expand(cwd=…)`, `loader.trickle`, the three steps `tmuxp load` runs before it builds |

`generate.py` never starts tmux. `generate.py --check` writes nothing and
exits 1 when a file differs from what tmuxp produces now, or when an example
and its expected file do not pair up. A pass is evidence only because it fails
on a changed file: edit a value in any `.json` and run it.

The files do not depend on the host. `generate.py` empties the environment and
sets the variables below before tmuxp runs, so `~` and `$VAR` expand to a
placeholder instead of this machine's value.

| Variable | Placeholder | Where the catalog reads it |
| --- | --- | --- |
| `HOME` | `/@HOME@` | `start_directory: "~"` |
| `PWD` | `/@PWD@` | `env-variables.yaml` |
| `MY_ENV_VAR` | `/@MY_ENV_VAR@` | `env-variables.yaml` |
| `USER` | `@USER@` | `env-variables.yaml` |
| `SERVER_PORT` | `@SERVER_PORT@` | `session-environment.yaml` |
| `MAIN_PANE_HEIGHT` | `@MAIN_PANE_HEIGHT@` | `options.yaml` |

The three path variables begin with a slash because tmuxp treats a
`start_directory` that begins with `~` or `/` as absolute, and joins any other
onto its parent. Any other variable stays literal, as it does in tmuxp when
the variable is unset. tmuxp resolves a leading `.` against the directory of
the workspace file; `generate.py` passes `/@WORKSPACE_DIR@`, which no example
reaches, because `start-directory.yaml` resolves its `./` against the session's
`/var/`.

A port's comparison sets the same variables to the same placeholders before it
loads a file, or replaces the value it used in its own output with the
placeholder, and does the same for the directory it loaded from.

## What is in a file

`expected.schema.json` is the contract, with each field described. In short:

- `session`, then `windows` in file order, then `panes` in file order. Every key
  is present; `null` means the file set nothing.
- Values are tmuxp's expanded ones. A pane's `shell_command_before` from the
  session, window and pane, then its own `shell_command`, are one `commands`
  list, each a `cmd` and the `enter` that applies to it. A bare string, a list,
  `blank`, `pane` and a lone `null` are already expanded, so a blank pane has no
  commands.
- A window's `start_directory` already has the session's joined on. A pane's
  `start_directory`, `shell` and `environment` are the ones that apply to it:
  its own, else its window's. A pane's `environment` replaces its window's
  rather than merging with it. The window's `shell` (`window_shell`) stays as
  written. The generator applies this rule itself, as it does the one below.
- Under `runtime`, at the session, pane and command level, are the keys tmuxp
  reads that act on timing, scripts, plugins or history, not on the tmux
  objects: `before_script`, `plugins`, `suppress_history`, `sleep_before` and
  `sleep_after`. A test that checks structure leaves them out of the
  comparison. The values for a pane and a command are the ones the builder
  applies, taken from `ClassicWorkspaceBuilder.iter_create_panes`: `enter` and
  the sleeps carry forward from a command to the ones after it, and
  `suppress_history` is the pane's, else the window's (the session's trickled
  down), else `true`.

`generate.py` computes those carried and inherited values itself, from that
loop, because `expand` and `trickle` stop before it. It is the one place the
generator reimplements tmuxp rather than calling it.

## Which syntax a port supports

`syntax-features.json` lists the tmuxp keys and forms the catalog uses, such as
`shell_command_before`, `window_shell`, `~` and `${VAR}` expansion, or a
layout name, with the examples that exercise each. `generate.py` derives the
list from the examples as written, so a feature with no example has an empty
`files` list: tmuxp supports it, and nothing here can say whether a port does.

Each port's catalog test writes a record of what it saw, validated by
[`syntax-support.schema.json`](syntax-support.schema.json), and commits it
beside its fixtures as `syntax-support.json`. The test records, for each
example, whether the port's loaded model matches the expected structure
(`matches`), differs in listed places (`deviates`) or refuses the file
(`refused`), and for each feature one of:

| Status | Meaning |
| --- | --- |
| `supported` | An example that exercises it loaded, and no difference or refusal is about it |
| `supported_with_difference` | It loads, and a recorded difference names it, with the reason |
| `refused` | The loader refuses a file that uses it, with the message |
| `unverified` | No example that loaded exercises it, so the test saw nothing either way |

A deviation or a refusal names the features it is about. Nothing writes the
record by hand: the test rebuilds it from what it observed and fails when the
committed file differs, so a record cannot claim support the test no longer
sees.

## Regenerating

Run the command above from this directory after a change to `generate.py`, or
after the examples in `..` move to another tmuxp release. Change `PINNED` in
`generate.py` and the version in this file together, and replace the examples
and their `LICENSE` from the same tag first.
