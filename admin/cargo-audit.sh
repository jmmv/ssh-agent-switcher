#! /bin/sh

is=
is="${is} RUSTSEC-2025-0069"  # `daemonize` is Unmaintained.

args="-D warnings"
for i in ${is}; do
    args="${args} --ignore ${i}"
done

set -x
# shellcheck disable=SC2086  # args requires word-splitting.
exec cargo audit ${args}
