# Major changes between releases

## Changes in version 1.0.1

**STILL UNDER DEVELOPMENT; NOT RELEASED YET**

*   No changes recorded.

## Changes in version 1.0.0

**Released on 2025-12-25.**

*   Added support for daemonization, making it easier (and more reliable)
    to start ssh-agent-switcher from login scripts.

*   Added a manual page.

*   Fixed long-standing issue where long agent responses with many keys
    locked up the ssh-agent-switcher due to short internal buffers.
    The proxying logic now supports partial reads and writes.

*   Rewrote the codebase (in Rust) to support adding new features and to
    simplify maintenance on my side.

*   Switched to a Make-based build system, dropping Bazel which was only
    really needed for test execution.

## Changes in version 0.0.0

**Never released.**

*   ssh-agent-switcher was first published on 2023-11-14 as a code dump
    and never had a formal release file.
