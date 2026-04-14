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

# 1. System deps
sudo apt-get update -qq
sudo apt-get install -y -qq \
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
jq -n --arg key "$FIREWORKS_API_KEY" '{"fireworks": {"type": "api", "key": $key}}' > ~/.config/opencode/auth.json
chmod 600 ~/.config/opencode/auth.json

# 5. Clone gamedata-recorder
cd ~
if [ ! -d gamedata-recorder ]; then
    git clone https://github.com/howardleegeek/gamedata-recorder.git
fi
cd gamedata-recorder
git pull

# 6. Install Rust toolchain (for cargo fmt/clippy on linter side)
if ! command -v cargo &>/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSfL --connect-timeout 30 --max-time 300 https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
fi
source "$HOME/.cargo/env" 2>/dev/null || true
if command -v rustup &>/dev/null; then
    rustup component add rustfmt clippy 2>/dev/null || true
fi

# 7. Verify
echo
echo "=== Verification ==="
echo "Bun:      $(bun --version 2>/dev/null || echo MISSING)"
echo "OpenCode: $(~/.opencode/bin/opencode --version 2>/dev/null || echo MISSING)"
echo "Cargo:    $(cargo --version 2>/dev/null || echo MISSING)"
echo "Repo:     $(git -C ~/gamedata-recorder log -1 --oneline 2>/dev/null || echo MISSING)"
echo "Memory:   $(free -h | awk '/^Mem:/{print $2}')"
echo "CPUs:     $(nproc)"
echo
echo "Setup complete. Ready to run autoresearch."
echo
echo "Usage:"
echo "  export FIREWORKS_API_KEY=<your_api_key>"
echo "  cd ~/gamedata-recorder"
echo "  ~/.opencode/bin/opencode run \"<your task>\""
