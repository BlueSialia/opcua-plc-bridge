#!/bin/sh
# Configures git to use the hooks in .githooks/.
set -e

HOOKS_DIR=".githooks"

if [ ! -d "$HOOKS_DIR" ]; then
    echo "No .githooks directory found. Run this from the repository root."
    exit 1
fi

git config core.hooksPath "$HOOKS_DIR"
echo "Git hooks path set to '$HOOKS_DIR'."
