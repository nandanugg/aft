# Shared by the fake adapters (sourced). Arguments: --language <lang> --root <root> (any order).
# The fakes print contract-v1 records for tests/adapters/fixtures/tiny-mixed (see outputs/).
# Bash and POSIX tools only.
FAKE_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
LANGUAGE=go
ROOT=$PWD
while [ $# -gt 0 ]; do
	case "$1" in
	--language) LANGUAGE=$2; shift 2 ;;
	--root) ROOT=$2; shift 2 ;;
	*) shift ;;
	esac
done
OUTPUT="$FAKE_DIR/outputs/tiny-$LANGUAGE.ndjson"
# All records except the end record, and the end record alone.
body() { grep -v '"type": "end"' "$OUTPUT"; }
end_record() { grep '"type": "end"' "$OUTPUT"; }
# Every run appends one line to $AFT_FAKE_RUN_LOG when it is set: "<fake> <language> <root> <pid>".
if [ -n "${AFT_FAKE_RUN_LOG:-}" ]; then echo "$(basename "$0") $LANGUAGE $ROOT $$" >> "$AFT_FAKE_RUN_LOG"; fi
