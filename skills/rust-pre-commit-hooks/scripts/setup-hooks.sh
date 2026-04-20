#!/usr/bin/env bash
#
# Setup script for Rust pre-commit hooks
# This script installs git pre-commit hooks that run cargo build and cargo fmt
#

set -e

echo "=== Rust Pre-Commit Hooks Setup ==="
echo ""

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Determine the script's directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOK_SOURCE="$SCRIPT_DIR/pre-commit.sh"
HOOK_TARGET=".git/hooks/pre-commit"

# Check if we're in a git repository
if [ ! -d ".git" ]; then
    echo -e "${RED}Error: .git directory not found${NC}"
    echo "Please run this script from the root of your git repository."
    exit 1
fi

# Check if Cargo.toml exists (Rust project check)
if [ ! -f "Cargo.toml" ]; then
    echo -e "${YELLOW}Warning: Cargo.toml not found${NC}"
    echo "This doesn't appear to be a Rust project root."
    echo "Continue anyway? (y/n)"
    read -r response
    if [[ ! "$response" =~ ^[Yy]$ ]]; then
        echo "Setup cancelled."
        exit 0
    fi
fi

# Check if source hook script exists
if [ ! -f "$HOOK_SOURCE" ]; then
    echo -e "${RED}Error: Hook source script not found at $HOOK_SOURCE${NC}"
    echo "Please ensure the skills/rust-pre-commit-hooks/scripts/ directory exists."
    exit 1
fi

# Check if hook already exists
if [ -f "$HOOK_TARGET" ]; then
    echo -e "${YELLOW}Warning: Pre-commit hook already exists at $HOOK_TARGET${NC}"
    echo ""
    echo "Current hook will be backed up to: ${HOOK_TARGET}.backup"
    cp "$HOOK_TARGET" "${HOOK_TARGET}.backup"
    echo ""
fi

# Copy the hook
echo "Installing pre-commit hook..."
cp "$HOOK_SOURCE" "$HOOK_TARGET"

# Make it executable
chmod +x "$HOOK_TARGET"

# Verify installation
if [ -x "$HOOK_TARGET" ]; then
    echo -e "${GREEN}✓ Pre-commit hook installed successfully${NC}"
    echo ""
    echo "Hook location: $HOOK_TARGET"
    echo ""
    echo "The hook will now run before every commit:"
    echo "  1. Runs 'cargo build' - blocks commit if build fails"
    echo "  2. Runs 'cargo fmt --check' - auto-fixes formatting issues"
    echo ""
    echo "To bypass the hook temporarily:"
    echo "  git commit --no-verify -m \"your message\""
    echo ""
    echo "To remove the hook:"
    echo "  rm $HOOK_TARGET"
else
    echo -e "${RED}✗ Failed to make hook executable${NC}"
    exit 1
fi
