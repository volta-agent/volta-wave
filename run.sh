#!/bin/bash
# volta-wave wrapper - handles terminal setup

if [ ! -t 0 ]; then
    echo "volta-wave requires an interactive terminal"
    echo "Run it directly in a terminal, not via script/pipe"
    exit 1
fi

exec /usr/local/bin/volta-wave "$@"
