#!/usr/bin/env bash
# Setup OpenCode + autoresearch deps on nucbox-wsl-1
# Run on the nucbox WSL machine itself (not Mac1)

set -euo pipefail

echo "=== GameData Recorder autoresearch setup on nucbox-wsl-1 ==="

# 1. System deps
sudo apt-get update -qq
sudo apt-get install -y -qq \
    git curl jq python3 python3-pip python3-venv \
    build-essential pkg-config libssl-dev \
    ripgrep

# 2. Install Bun (OpenCode runtime)
if ! command -v bun &>/dev/null; then
    curl -fsSL https://bun.sh/install | bash
    export PATH="$HOME/.bun/bin:$PATH"
fi

# 3. Install OpenCode
if [ ! -d "$HOME/.opencode" ]; then
    curl -fsSL https://opencode.ai/install | bash
fi
export PATH="$HOME/.opencode/bin:$PATH"

# 4. Configure Fireworks API key (Kimi K2.5 Turbo)
mkdir -p ~/.config/opencode
cat > ~/.config/opencode/auth.json <<'EOF'
{
  "fireworks": {
    "type": "api",
    "key": "fw_GqRyWohrw849BSZ6xRNuyA"
  }
}
EOF

# 5. Clone gamedata-recorder
cd ~
if [ ! -d gamedata-recorder ]; then
    git clone https://github.com/howardleegeek/gamedata-recorder.git
fi
cd gamedata-recorder
git pull

# 6. Install Rust toolchain (for cargo fmt/clippy on linter side)
if ! command -v cargo &>/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSfL https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    source "$HOME/.cargo/env"
    rustup component add rustfmt clippy
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
echo "  cd ~/gamedata-recorder"
echo "  FIREWORKS_API_KEY=fw_GqRyWohrw849BSZ6xRNuyA \\"
echo "    ~/.opencode/bin/opencode run \"<your task>\""
