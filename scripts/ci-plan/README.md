# ci-plan

Plans the tmux lanes of the `CI` workflow from
[`.github/ci-matrix.txt`](../../.github/ci-matrix.txt), and checks that every
job the plan put in scope passed. No planner is shared between the ports, so
this one is this repository's own.

The `plan` job runs it first, and the `compat` job takes its matrix from the
output. The `required` job runs `verify` last, so a job the plan left out of
scope may be skipped and any other must have succeeded.

| Event | Lanes |
| --- | --- |
| Push or pull request | The first and last supported release |
| Weekly schedule, or a dispatch with `scope=full` | Every lane in the manifest, and the `fuzz` and `bench` jobs |
| Dispatch with `scope=custom` | The `tmux-ref` list: versions, tags, branches or commits |

A dispatch reads `INPUT_SCOPE`, `INPUT_TMUX_REF`, `INPUT_RUST_TOOLCHAIN`,
`INPUT_OS` and `INPUT_TMATE`. Any other event ignores them.

Plan a pull request from the checked-in manifest:

```console
$ GITHUB_EVENT_NAME=pull_request uv run scripts/ci-plan/ci_plan.py
```

Check a `needs` result the way the `required` job does:

```console
$ NEEDS_JSON='{"plan":{"result":"success"}}' \
    uv run scripts/ci-plan/ci_plan.py verify
```

Run its tests, which include the checks that hold the workflows to the plan:

```console
$ uv run --project scripts pytest scripts/ci-plan
```
