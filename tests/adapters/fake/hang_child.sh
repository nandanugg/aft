#!/usr/bin/env bash
# Starts a sleeping child (it stays in the adapter's process group), prints the header, then waits forever.
# After the runner's timeout no process "sleep 86398" may be left (check with pgrep -f).
. "$(dirname "$0")/common.sh"
sleep 86398 &
head -n 1 "$OUTPUT"
wait
