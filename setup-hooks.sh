#!/bin/sh
# Configure Git to use the hooks in .githooks/.
set -eu

if [ ! -d .githooks ]; then
    echo "No .githooks directory found. Run this from the repository root." >&2
    exit 1
fi

git config core.hooksPath .githooks
echo "Git hooks path set to '.githooks'."
