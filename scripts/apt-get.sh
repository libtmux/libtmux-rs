#!/usr/bin/env bash
# Run apt-get on a GitHub-hosted runner without letting one hung mirror stall
# the job.
#
# Why: actions/runner-images#15040. On ubuntu-24.04 images 20260927, 20261004
# and 20261012, `apt-get update` sometimes ignores the Azure mirror and falls
# back to https://archive.ubuntu.com/ubuntu, then hangs until the job timeout.
# The image's 15 s Acquire timeout does not apply after the failover.
#
# What: run `apt-get -o Acquire::Retries=3 "$@"` as root under `timeout`. When
# an attempt fails or times out, move the first line of
# /etc/apt/apt-mirrors.txt to the end so the next attempt starts on another
# mirror, and try again, up to three attempts in all.
#
# Usage: apt-get.sh update
#        apt-get.sh install --yes --no-install-recommends tmux
# Env:   APT_TIMEOUT  seconds one attempt may take (default 180)
set -u

mirrors=/etc/apt/apt-mirrors.txt
attempts=3

for attempt in $(seq 1 "$attempts"); do
  if sudo timeout --kill-after=10 "${APT_TIMEOUT:-180}" apt-get -o Acquire::Retries=3 "$@"; then
    exit 0
  fi
  echo "apt-get: attempt ${attempt} of ${attempts} failed" >&2
  if [ "$attempt" -lt "$attempts" ] && [ -s "$mirrors" ]; then
    rotated=$({ tail -n +2 "$mirrors"; head -n 1 "$mirrors"; })
    printf '%s\n' "$rotated" | sudo tee "$mirrors" >/dev/null
  fi
done
exit 1
