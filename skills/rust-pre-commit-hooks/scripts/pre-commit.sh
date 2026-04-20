#!/usr/bin/env bash
#
# Git pre-commit hook for Rust projects
# Runs cargo build and cargo fmt before allowing commit
#

set -e

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${YELLOW}Running pre-commit hook...${NC}"

# Store the hook's output in a temp file
TMPFILE=$(mktemp)
trap "rm -f $TMPFILE" EXIT

# Function to run a check and report result
run_check() {
    local name="$1"
    local command="$2"
    local fix_command="$3"

    echo -n "  $name... "

    if eval "$command" > "$TMPFILE" 2>&1; then
        echo -e "${GREEN}✓ passed${NC}"
        return 0
    else
        echo -e "${RED}✗ failed${NC}"
        cat "$TMPFILE"
        rm -f "$TMPFILE"

        # If there's a fix command, run it
        if [ -n "$fix_command" ]; then
            echo -e "${YELLOW}Attempting to fix...${NC}"
            if eval "$fix_command" > "$TMPFILE" 2>&1; then
                echo -e "${GREEN}✓ Fixed!${NC}"
                # Recheck after fixing
                if eval "$command" > "$TMPFILE" 2>&1; then
                    return 0
                else
                    echo -e "${RED}✗ Still failing after fix attempt${NC}"
                    cat "$TMPFILE"
                    return 1
                fi
            else
                echo -e "${RED}✗ Fix attempt failed${NC}"
                cat "$TMPFILE"
                return 1
            fi
        fi

        return 1
    fi
}

# Run checks
BUILD_FAILED=false
FORMAT_FAILED=false

# Check 1: Build (using cargo check to avoid linker stack overflow on Windows)
if ! run_check "Build check" "cargo check"; then
    BUILD_FAILED=true
fi

# Check 2: Formatting (with auto-fix)
if ! run_check "Formatting check" "cargo fmt --check" "cargo fmt"; then
    FORMAT_FAILED=true
fi

# Summary
echo ""
if [ "$BUILD_FAILED" = true ] || [ "$FORMAT_FAILED" = true ]; then
    echo -e "${RED}=== Pre-commit hook failed ===${NC}"
    echo ""
    echo "Your commit was blocked due to the errors above."

    if [ "$BUILD_FAILED" = true ]; then
        echo ""
        echo -e "${YELLOW}Build errors:${NC}"
        echo "  - Fix compilation errors"
        echo "  - Run 'cargo build' to see full errors"
        echo "  - Ensure all dependencies are in Cargo.toml"
    fi

    if [ "$FORMAT_FAILED" = true ]; then
        echo ""
        echo -e "${YELLOW}Formatting issues:${NC}"
        echo "  - Auto-fix attempted with 'cargo fmt'"
        echo "  - Review the changes with 'git diff'"
        echo "  - Stage the formatting fixes: 'git add -u'"
        echo "  - Then commit again"
    fi

    echo ""
    echo "To bypass this hook (not recommended):"
    echo "  git commit --no-verify -m \"your message\""
    echo ""

    exit 1
fi

echo -e "${GREEN}=== All checks passed ===${NC}"
echo ""
echo "Proceeding with commit..."
exit 0
