#!/usr/bin/env bash
# Sleeps $FAKE_SLEEP_S seconds (default 25, more than the 20 s query wait), then prints a valid run.
. "$(dirname "$0")/common.sh"
sleep "${FAKE_SLEEP_S:-25}"
cat "$OUTPUT"
