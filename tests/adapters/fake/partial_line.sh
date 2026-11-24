#!/usr/bin/env bash
# The last line (the end record) has no newline: the output was cut off.
. "$(dirname "$0")/common.sh"
body
printf '%s' "$(end_record)"
