#! /bin/sh
# Copyright 2024 Julio Merino.
# All rights reserved.
#
# Redistribution and use in source and binary forms, with or without
# modification, are permitted provided that the following conditions are
# met:
#
# * Redistributions of source code must retain the above copyright
#   notice, this list of conditions and the following disclaimer.
# * Redistributions in binary form must reproduce the above copyright
#   notice, this list of conditions and the following disclaimer in the
#   documentation and/or other materials provided with the distribution.
# * Neither the name of Google Inc. nor the names of its contributors
#   may be used to endorse or promote products derived from this software
#   without specific prior written permission.
#
# THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
# "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
# LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
# A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
# OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
# SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
# LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
# DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
# THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
# (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
# OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

set -eux

err() {
    echo "${0}: ${*}" 1>&2
    exit 1
}

check_srcs() {
    local makefile_srcs
    makefile_srcs="$(grep '^SRCS = ' Makefile | cut -d ' ' -f 3-)"
    [ -n "${makefile_srcs}" ] || err "Cannot determine SRCS from Makefile"

    local actual_srcs
    actual_srcs="$(echo $(find src -type f | sort))"

    [ "${makefile_srcs}" = "${actual_srcs}" ] \
        || err "Makefile SRCS seems to be out of date"
}

check_clean() {
    make clean
    [ -z "$(git clean -xdfn 2>&1)" ] \
        || err "make clean does not remove all files"
}

check_install() {
    local root="$(mktemp -d)"
    trap "rm -rf '${root}'" EXIT

    make install PREFIX="${root}"
    "${root}/bin/ssh-agent-switcher" -h 2>&1 | grep 'Usage: ssh-agent-switcher'
    local debug_size="$(stat -c '%s' "${root}/bin/ssh-agent-switcher")"

    make install MODE=release PREFIX="${root}"
    "${root}/bin/ssh-agent-switcher" -h 2>&1 | grep 'Usage: ssh-agent-switcher'
    local release_size="$(stat -c '%s' "${root}/bin/ssh-agent-switcher")"

    [ "${release_size}" -lt "${debug_size}" ] \
        || err "Release binary is larger than debug binary"
}

check_test() {
    make test
}

main() {
    check_srcs
    check_test
    check_install
    check_clean
}

main "${@}"
