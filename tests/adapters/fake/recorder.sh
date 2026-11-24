#!/usr/bin/env bash
# Binding tests: records how it was called, then prints a valid run for its --language.
# Writes $AFT_FAKE_RECORD_DIR/record-<language>-<pid>.txt with: argv lines, pwd, stdin kind, the
# process group id, and the full environment (sorted), so a test can check which adapter ran,
# with which root, language and env.
. "$(dirname "$0")/common.sh"
if [ -z "${AFT_FAKE_RECORD_DIR:-}" ]; then cat "$OUTPUT"; exit 0; fi  # nothing to record into
file="$AFT_FAKE_RECORD_DIR/record-$LANGUAGE-$$.txt"
{
	i=0
	for a in "$0" "$@"; do echo "argv[$i]=$a"; i=$((i + 1)); done
	echo "pwd=$PWD"
	echo "language=$LANGUAGE"
	echo "root=$ROOT"
	if [ -t 0 ]; then echo "stdin=tty"; elif [ -p /dev/stdin ]; then echo "stdin=pipe"; else echo "stdin=file-or-null"; fi
	echo "pgid=$(ps -o pgid= -p $$ | tr -d ' ')"
	echo "pid=$$"
	env | LC_ALL=C sort | sed 's/^/env:/'
} > "$file"
cat "$OUTPUT"
