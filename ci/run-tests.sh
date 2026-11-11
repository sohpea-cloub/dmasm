#!/bin/sh
# Runs test programs that ci/build-tests.sh built:
#
#     ci/run-tests.sh DIRECTORY
#
# Run it from the repository's root, where cargo would run them. Programs
# built for Windows run as they are on Windows, and on Linux under Wine when
# WINE names it (WINE=wine).
set -u
[ $# -eq 1 ] || { sed -n '2,8p' "$0" >&2; exit 2; }
[ -z "${WINE:-}" ] || export WINEDEBUG="${WINEDEBUG:--all}"

failed=
for test in "$1"/*; do
	name=$(basename "$test" .exe)
	echo "::group::$name"
	${WINE:+"$WINE"} "$test"
	status=$?
	echo "::endgroup::"
	if [ $status -eq 0 ]; then
		echo "passed: $name"
	else
		echo "FAILED: $name (exit status $status)"
		failed="$failed $name"
	fi
done

if [ -n "$failed" ]; then
	echo "failed:$failed"
	exit 1
fi
echo "all passed"
