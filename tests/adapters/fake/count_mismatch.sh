#!/usr/bin/env bash
# The end record counts one site more than printed.
. "$(dirname "$0")/common.sh"
body
end_record | sed 's/"sites": \([0-9]*\)/"sites": 9\1/'
