# Copyright 2023 Julio Merino.
# All rights reserved.
#
# Redistribution and use in source and binary forms, with or without modification, are permitted
# provided that the following conditions are met:
#
# * Redistributions of source code must retain the above copyright notice, this list of conditions
#   and the following disclaimer.
# * Redistributions in binary form must reproduce the above copyright notice, this list of
#   conditions and the following disclaimer in the documentation and/or other materials provided with
#   the distribution.
# * Neither the name of rules_shtk nor the names of its contributors may be used to endorse or
#   promote products derived from this software without specific prior written permission.
#
# THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR
# IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND
# FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR
# CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
# DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
# DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
# WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY
# WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

shtk_import unittest

shtk_unittest_add_fixture standalone
standalone_fixture() {
    setup() {
        # Unix domain socket names have tight length limitations so we must place them under
        # /tmp (instead of the current work directory, which would be preferrable because then
        # we would get automatic cleanup).
        SOCKETS_ROOT="$(mktemp -d -p /tmp)"
    }

    teardown() {
        [ ! -e pid ] || kill "$(cat pid)"

        rm -rf "${SOCKETS_ROOT}"
    }

    shtk_unittest_add_test default_socket_path
    default_socket_path_test() {
        USER=fake-user ../ssh-agent-switcher_/ssh-agent-switcher 2>switcher.log &
        echo "${!}" >pid  # For teardown.

        while [ ! -e /tmp/ssh-agent.fake-user ]; do
            sleep 0.01
        done
    }

    shtk_unittest_add_test ignore_sighup
    ignore_sighup_test() {
        local socket="${SOCKETS_ROOT}/socket"
        ../ssh-agent-switcher_/ssh-agent-switcher --socketPath "${socket}" 2>switcher.log &
        local pid="$!"
        echo "${pid}" >pid  # For teardown.

        # Wait for the socket to appear.
        while [ ! -e "${socket}" ]; do
            sleep 0.01
        done

        kill -HUP "${pid}"

        # Wait a little bit to see if the socket is cleared.  This is racy and may fail to detect
        # a legitimate bug, but it should not raise a false failure.
        local i=0
        while [ "${i}" -lt 10 ]; do
            [ -e "${socket}" ] || fail "Daemon exited and deleted file"
            sleep 0.01
            i=$((i + 1))
        done
    }
}

shtk_unittest_add_fixture integration
integration_fixture() {
    setup() {
        # Unix domain socket names have tight length limitations so we must place them under
        # /tmp (instead of the current work directory, which would be preferrable because then
        # we would get automatic cleanup).
        SOCKETS_ROOT="$(mktemp -d -p /tmp)"

        # Place the agent socket under an ssh-* directory that sorts last.  We need this for
        # the unknown files test.
        AGENT_AUTH_SOCK="${SOCKETS_ROOT}/ssh-zzz/agent.bar"

        mkdir -p "$(dirname "${AGENT_AUTH_SOCK}")"
        ssh-agent -a "${AGENT_AUTH_SOCK}" >agent.env

        SWITCHER_AUTH_SOCK="${SOCKETS_ROOT}/switcher"
        ../ssh-agent-switcher_/ssh-agent-switcher \
            --socketPath "${SWITCHER_AUTH_SOCK}" \
            --agentsDir "${SOCKETS_ROOT}" \
            2>switcher.log &
        SWITCHER_AGENT_PID="${!}"

        export SSH_AUTH_SOCK="${SWITCHER_AUTH_SOCK}"
    }

    teardown() {
        # Check that the expected real agent was used.
        expect_file match:"opened.*${AGENT_AUTH_SOCK}" switcher.log
        # Check that we didn't leave an open connection behind due to EOF mishandling.
        expect_file match:"Closing client connection" switcher.log

        kill "${SWITCHER_AGENT_PID}"
        # Make sure the daemon deletes the socket on exit.
        while [ -e "${SWITCHER_AUTH_SOCK}" ]; do
            sleep 0.01
        done
        expect_file match:"Shutting down.*${SWITCHER_AUTH_SOCK}" switcher.log

        . agent.env
        kill "${SSH_AGENT_PID}"

        rm -rf "${SOCKETS_ROOT}"
    }

    shtk_unittest_add_test list_identities
    list_identities_test() {
        expect_command -s 1 -o match:"no identities" ssh-add -l
    }

    shtk_unittest_add_test add_identity
    add_identity_test() {
        assert_command -s 0 -o ignore -e ignore ssh-keygen -t rsa -b 1024 -N '' -f ./id_rsa
        expect_command -s 0 -e match:"Identity added" ssh-add ./id_rsa
    }

    shtk_unittest_add_test ignore_unknown_files
    ignore_unknown_files_test() {
        # Create garbage in the sockets directory.
        touch "${SOCKETS_ROOT}/file-unknown"
        mkdir "${SOCKETS_ROOT}/dir-unknown"
        touch "${SOCKETS_ROOT}/ssh-not-a-dir"
        mkdir "${SOCKETS_ROOT}/ssh-empty"
        mkdir "${SOCKETS_ROOT}/ssh-foo"
        touch "${SOCKETS_ROOT}/ssh-foo/unknown"
        mkdir "${SOCKETS_ROOT}/ssh-bar"
        touch "${SOCKETS_ROOT}/ssh-bar/agent.not-a-socket"

        expect_command -s 1 -o match:"no identities" ssh-add -l

        # Ensure that the garbage was ignored for the correct reasons.
        expect_file match:"Ignoring.*/file-unknown.*not a directory" switcher.log
        expect_file match:"Ignoring.*/dir-unknown.*not start with.*ssh-" switcher.log
        expect_file match:"Ignoring.*/ssh-not-a-dir.*not a directory" switcher.log
        expect_file match:"Ignoring.*/ssh-empty.*no socket" switcher.log
        expect_file match:"Ignoring.*/ssh-foo/unknown.*start with.*agent" switcher.log
        expect_file match:"Ignoring.*/ssh-bar/agent.not-a-socket.*open failed" switcher.log
    }
}
