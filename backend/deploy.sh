#!/bin/bash
# Deploy GameData Labs backend to any Linux server
# Usage: bash deploy.sh [server_host]

set -e

SERVER="${1:-localhost}"
PORT="${PORT:-8080}"

echo "=== GameData Labs Backend Deploy ==="

# Check Python
if ! command -v python3 &> /dev/null; then
    echo "Installing Python..."
    sudo apt-get update && sudo apt-get install -y python3 python3-pip python3-venv
fi

# Create virtual environment
DEPLOY_DIR="$HOME/gamedata-backend"
mkdir -p "$DEPLOY_DIR"

if [ ! -d "$DEPLOY_DIR/venv" ]; then
    python3 -m venv "$DEPLOY_DIR/venv"
fi

# Install dependencies
"$DEPLOY_DIR/venv/bin/pip" install -q -r requirements.txt

# Copy files
cp main.py "$DEPLOY_DIR/"
cp models.py "$DEPLOY_DIR/"
cp requirements.txt "$DEPLOY_DIR/"

# Create .env if not exists
if [ ! -f "$DEPLOY_DIR/.env" ]; then
    # Generate a cryptographically secure random API secret
    # Using openssl for portability - produces 32 bytes of random hex (64 chars)
    GENERATED_SECRET=$(openssl rand -hex 32 2>/dev/null || python3 -c "import secrets; print(secrets.token_hex(32))")
    cat > "$DEPLOY_DIR/.env" << ENV
API_SECRET=$GENERATED_SECRET
S3_BUCKET=gamedata-recordings
S3_REGION=us-east-1
# AWS_ACCESS_KEY_ID=
# AWS_SECRET_ACCESS_KEY=
DATA_DIR=/var/lib/gamedata
PORT=8080
ENV
    echo "Created .env file at $DEPLOY_DIR/.env with auto-generated API_SECRET"
    echo "⚠️  Review other settings in the .env file before production use"
fi

# Create systemd service
sudo tee /etc/systemd/system/gamedata-backend.service > /dev/null << EOF
[Unit]
Description=GameData Labs Backend API
After=network.target

[Service]
Type=simple
User=$USER
WorkingDirectory=$DEPLOY_DIR
EnvironmentFile=$DEPLOY_DIR/.env
ExecStart=$DEPLOY_DIR/venv/bin/uvicorn main:app --host 0.0.0.0 --port $PORT
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable gamedata-backend
sudo systemctl restart gamedata-backend

echo "=== Backend deployed at http://$SERVER:$PORT ==="
echo "Health check: curl http://$SERVER:$PORT/health"
echo "Logs: journalctl -u gamedata-backend -f"
