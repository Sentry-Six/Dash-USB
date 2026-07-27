#!/bin/bash -eu

ping -q -w "${ARCHIVE_PING_TIMEOUT:-1}" -c 1 "$1" > /dev/null 2>&1
