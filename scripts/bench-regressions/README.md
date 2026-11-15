# bench-regressions

Compares each criterion benchmark with the run before it and fails on a
doubling. Two runs rarely share a machine, so each benchmark is judged against
the median change of all of them, the machine factor, and fails only when it
is `--limit` times slower than its neighbours moved. A factor past
`--machine-limit` fails on its own. The first run has nothing to compare with
and passes. Every change is printed, and appended to `$GITHUB_STEP_SUMMARY`
when that is set.

The weekly `benchmarks` CI job runs it after `just bench` and `just
bench-waits`, and `just bench-compare` runs it by hand.

```console
$ uv run scripts/bench-regressions/bench_regressions.py target/criterion
```

Run its tests:

```console
$ uv run pytest scripts/bench-regressions
```
