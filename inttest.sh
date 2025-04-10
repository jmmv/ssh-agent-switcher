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

        # Create a mock process directory and file to simulate an sshd process with PTS
        # This will be used by the process validation functions
        MOCK_PID=$$  # Use our own PID for simplicity
        mkdir -p "${SOCKETS_ROOT}/proc/${MOCK_PID}"
        echo "sshd: user@pts/1" > "${SOCKETS_ROOT}/proc/${MOCK_PID}/cmdline"

        # Place the agent socket under an ssh-* directory that sorts last.  We need this for
        # the unknown files test.
        AGENT_AUTH_SOCK="${SOCKETS_ROOT}/ssh-zzz/agent.${MOCK_PID}"

        mkdir -p "$(dirname "${AGENT_AUTH_SOCK}")"
        ssh-agent -a "${AGENT_AUTH_SOCK}" >agent.env

        # Override the process.go functions to use our mock directory
        export PROCESS_OVERRIDE_PROC_DIR="${SOCKETS_ROOT}/proc"

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

        # Store the agent env filenames in an array for cleanup
        AGENT_ENV_FILES=()

        # Start dummy agent w/o process for invalid socket path test
        mkdir -p "${SOCKETS_ROOT}/ssh-invalid-pid"
        ssh-agent -a "${SOCKETS_ROOT}/ssh-invalid-pid/agent.xyz" >invalid_pid.env
        AGENT_ENV_FILES+=("invalid_pid.env")

        # Start dummy agent w/o process for no process test
        mkdir -p "${SOCKETS_ROOT}/ssh-no-process"
        ssh-agent -a "${SOCKETS_ROOT}/ssh-no-process/agent.99999" >no_process.env
        AGENT_ENV_FILES+=("no_process.env")

        # Create a mock process without a PTS
        NO_PTS_PID=$((MOCK_PID + 1))
        mkdir -p "${SOCKETS_ROOT}/proc/${NO_PTS_PID}"
        echo "sshd: user@notty" > "${SOCKETS_ROOT}/proc/${NO_PTS_PID}/cmdline"
        mkdir -p "${SOCKETS_ROOT}/ssh-no-pts"
        ssh-agent -a "${SOCKETS_ROOT}/ssh-no-pts/agent.${NO_PTS_PID}" >no_pts.env
        AGENT_ENV_FILES+=("no_pts.env")

        # Create a regular file with the name of a valid socket (no agent involved)
        NOT_A_SOCKET=$((MOCK_PID + 2))
        mkdir -p "${SOCKETS_ROOT}/proc/${NOT_A_SOCKET}"
        echo "sshd: user@pts/1" > "${SOCKETS_ROOT}/proc/${NOT_A_SOCKET}/cmdline"
        mkdir -p "${SOCKETS_ROOT}/ssh-not-a-socket"
        touch "${SOCKETS_ROOT}/ssh-not-a-socket/agent.${NOT_A_SOCKET}"

        expect_command -s 1 -o match:"no identities" ssh-add -l

        # Ensure that the garbage was ignored for the correct reasons.
        expect_file match:"Ignoring.*/file-unknown.*not a directory" switcher.log
        expect_file match:"Ignoring.*/dir-unknown.*not start with.*ssh-" switcher.log
        expect_file match:"Ignoring.*/ssh-not-a-dir.*not a directory" switcher.log
        expect_file match:"Ignoring.*/ssh-empty.*no socket" switcher.log
        expect_file match:"Ignoring.*/ssh-foo/unknown.*start with.*agent" switcher.log
        expect_file match:"Ignoring.*/ssh-not-a-socket/agent.${NOT_A_SOCKET}.*not a socket" switcher.log

        # Check new validation messages
        expect_file match:"Ignoring.*/ssh-invalid-pid/agent.xyz.*invalid socket path" switcher.log
        expect_file match:"Ignoring.*/ssh-no-process/agent.99999.*not owned by sshd process" switcher.log
        expect_file match:"Ignoring.*/ssh-no-pts/agent.${NO_PTS_PID}.*does not have a PTS attached" switcher.log

        # Kill all the ssh-agent processes we started
        for env_file in "${AGENT_ENV_FILES[@]}"; do
            . "${env_file}"
            kill "${SSH_AGENT_PID}"
        done

    }
}
