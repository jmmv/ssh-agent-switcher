# Copyright 2025 Julio Merino
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
# * Neither the name of ssh-agent-switcher nor the names of its
#   contributors may be used to endorse or promote products derived from
#   this software without specific prior written permission.
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

PREFIX = /usr/local
MODE = debug

BIN = target/$(MODE)/ssh-agent-switcher
RS_SRCS = src/find.rs src/lib.rs src/main.rs
SRCS = Cargo.toml $(RS_SRCS)

.PHONY: all
all: $(BIN)

$(BIN): $(SRCS)
	@if [ "$(MODE)" = debug ]; then arg=; else arg=--$(MODE); fi; \
	    echo cargo build $${arg}; \
	    cargo build $${arg}

.PHONY: test
test: $(BIN) inttest
	MODE=$(MODE) ./inttest

inttest: inttest.sh
	shtk build -m shtk_unittest_main -o $@ inttest.sh

.PHONY: install
install: $(BIN)
	install -m 755 -d "$(DESTDIR)$(PREFIX)/bin"
	install -m 755 "$(BIN)" "$(DESTDIR)$(PREFIX)/bin/ssh-agent-switcher"
	install -m 755 -d "$(DESTDIR)$(PREFIX)/share/doc/ssh-agent-switcher"
	install -m 644 COPYING "$(DESTDIR)$(PREFIX)/share/doc/ssh-agent-switcher/COPYING"
	install -m 644 NEWS.md "$(DESTDIR)$(PREFIX)/share/doc/ssh-agent-switcher/NEWS.md"
	install -m 644 README.md "$(DESTDIR)$(PREFIX)/share/doc/ssh-agent-switcher/README.md"
	install -m 755 -d "$(DESTDIR)$(PREFIX)/share/man/man1"
	install -m 644 ssh-agent-switcher.1 "$(DESTDIR)$(PREFIX)/share/man/man1/ssh-agent-switcher.1"

.PHONY: clean
clean:
	cargo clean
	rm -f Cargo.lock inttest
