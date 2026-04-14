#!/usr/bin/env bash
# Setup OpenCode + autoresearch deps on nucbox-wsl-1
# Run on the nucbox WSL machine itself (not Mac1)

set -euo pipefail

# Check for required API key
if [ -z "${FIREWORKS_API_KEY:-}" ]; then
    echo "Error: FIREWORKS_API_KEY environment variable is required"
    echo "Usage: FIREWORKS_API_KEY=<your_key> $0"
    exit 1
fi

echo "=== GameData Recorder autoresearch setup on nucbox-wsl-1 ==="

# 1. System deps (with timeout to prevent indefinite hangs)
export DEBIAN_FRONTEND=noninteractive
APT_OPTIONS="-o Acquire::Timeout=60 -o Acquire::Retries=3"
sudo apt-get update -qq $APT_OPTIONS
sudo apt-get install -y -qq $APT_OPTIONS \
    git curl jq python3 python3-pip python3-venv \
    build-essential pkg-config libssl-dev \
    ripgrep procps

# 2. Install Bun (OpenCode runtime)
if ! command -v bun &>/dev/null; then
    curl -fsSL --connect-timeout 30 --max-time 300 https://bun.sh/install | bash
fi
export PATH="$HOME/.bun/bin:$PATH"

# 3. Install OpenCode
if [ ! -d "$HOME/.opencode" ]; then
    curl -fsSL --connect-timeout 30 --max-time 300 https://opencode.ai/install | bash
fi
export PATH="$HOME/.opencode/bin:$PATH"

# 4. Configure Fireworks API key (Kimi K2.5 Turbo)
mkdir -p ~/.config/opencode
chmod 700 ~/.config/opencode
(umask 077 && jq -n --arg key "$FIREWORKS_API_KEY" '{"fireworks": {"type": "api", "key": $key}}' > ~/.config/opencode/auth.json)
if [ ! -f ~/.config/opencode/auth.json ] || ! jq -e . ~/.config/opencode/auth.json >/dev/null 2>&1; then
    echo "Error: Failed to create valid auth.json"
    exit 1
fi

# 5. Clone gamedata-recorder
cd ~
if [ ! -d gamedata-recorder ]; then
    if ! git clone https://github.com/howardleegeek/gamedata-recorder.git; then
        echo "Error: Failed to clone gamedata-recorder repository"
        exit 1
    fi
fi
cd gamedata-recorder
if ! git pull 2>/dev/null; then
    echo "Warning: git pull failed, continuing with local version"
fi

# 6. Install Rust toolchain (for cargo fmt/clippy on linter side)
if ! command -v cargo &>/dev/null; then
    if ! curl --proto '=https' --tlsv1.2 -sSfL --connect-timeout 30 --max-time 300 https://sh.rustup.rs | sh -s -- -y --default-toolchain stable; then
        echo "Error: Failed to install Rust toolchain"
        exit 1
    fi
fi
source "$HOME/.cargo/env" 2>/dev/null || true
if command -v rustup &>/dev/null; then
    rustup component add rustfmt clippy 2>/dev/null || true
fi

# 7. Verify
echo
echo "=== Verification ==="
BUN_VERSION=$(command -v bun &>/dev/null && bun --version 2>/dev/null || echo "MISSING")
OPENCODE_VERSION=$(command -v opencode &>/dev/null && opencode --version 2>/dev/null || $HOME/.opencode/bin/opencode --version 2>/dev/null || echo "MISSING")
CARGO_VERSION=$(cargo --version 2>/dev/null || echo "MISSING")
JQ_VERSION=$(jq --version 2>/dev/null || echo "MISSING")
REPO_STATUS=$(git -C ~/gamedata-recorder log -1 --oneline 2>/dev/null || echo "MISSING")
echo "Bun:      $BUN_VERSION"
echo "OpenCode: $OPENCODE_VERSION"
echo "Cargo:    $CARGO_VERSION"
echo "jq:       $JQ_VERSION"
echo "Repo:     $REPO_STATUS"
echo "Memory:   $(free -h 2>/dev/null | awk '/^Mem:/{print $2}' || echo "N/A")"
echo "CPUs:     $(nproc 2>/dev/null || echo "N/A")"

# Check critical tools are present
CRITICAL_MISSING=""
if [ "$BUN_VERSION" = "MISSING" ]; then
    CRITICAL_MISSING="$CRITICAL_MISSING bun"
fi
if [ "$OPENCODE_VERSION" = "MISSING" ]; then
    CRITICAL_MISSING="$CRITICAL_MISSING opencode"
fi
if [ -n "$CRITICAL_MISSING" ]; then
    echo
    echo "Error: Critical tools missing:$CRITICAL_MISSING"
    exit 1
fi

echo
echo "Setup complete. Ready to run autoresearch."
echo
echo "Usage:"
echo "  export FIREWORKS_API_KEY=<your_api_key>"
echo "  cd ~/gamedata-recorder"
echo "  ~/.opencode/bin/opencode run \"<your task>\""
