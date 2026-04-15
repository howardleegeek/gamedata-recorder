#!/bin/bash
# Database setup script for GameData Labs backend
# Usage: ./setup_database.sh [environment]

set -e

ENVIRONMENT="${1:-development}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "============================================================"
echo "🎮 GameData Labs - Database Setup"
echo "============================================================"
echo "Environment: $ENVIRONMENT"
echo "============================================================"

# Check if PostgreSQL is installed
if ! command -v psql &> /dev/null; then
    echo "❌ PostgreSQL is not installed. Please install PostgreSQL first."
    echo "   macOS: brew install postgresql"
    echo "   Ubuntu: sudo apt-get install postgresql postgresql-contrib"
    exit 1
fi

# Database configuration
DB_NAME="${DB_NAME:-gamedata}"
DB_USER="${DB_USER:-gamedata}"
DB_PASSWORD="${DB_PASSWORD:-gamedata}"
DB_HOST="${DB_HOST:-localhost}"
DB_PORT="${DB_PORT:-5432}"

# Create database and user
echo ""
echo "📦 Setting up database..."

# Check if database exists
if psql -h "$DB_HOST" -p "$DB_PORT" -U postgres -lqt | cut -d \| -f 1 | grep -qw "$DB_NAME"; then
    echo "⚠️  Database '$DB_NAME' already exists"
    read -p "   Do you want to drop and recreate it? (y/N): " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        echo "   Dropping database..."
        dropdb -h "$DB_HOST" -p "$DB_PORT" -U postgres "$DB_NAME"
        echo "   Creating database..."
        createdb -h "$DB_HOST" -p "$DB_PORT" -U postgres "$DB_NAME"
    fi
else
    echo "   Creating database '$DB_NAME'..."
    createdb -h "$DB_HOST" -p "$DB_PORT" -U postgres "$DB_NAME" || true
fi

# Check if user exists
if psql -h "$DB_HOST" -p "$DB_PORT" -U postgres -tc "SELECT 1 FROM pg_roles WHERE rolname='$DB_USER'" | grep -q 1; then
    echo "⚠️  User '$DB_USER' already exists"
else
    echo "   Creating user '$DB_USER'..."
    # Escape single quotes in password for SQL (PostgreSQL uses '' to escape ')
    DB_PASSWORD_ESCAPED=$(echo "$DB_PASSWORD" | sed "s/'/''/g")
    psql -h "$DB_HOST" -p "$DB_PORT" -U postgres -c "CREATE USER $DB_USER WITH PASSWORD '$DB_PASSWORD_ESCAPED';"
fi

# Grant privileges
echo "   Granting privileges..."
psql -h "$DB_HOST" -p "$DB_PORT" -U postgres -c "GRANT ALL PRIVILEGES ON DATABASE $DB_NAME TO $DB_USER;"

# Install required Python packages
echo ""
echo "📦 Installing Python dependencies..."
pip install -q sqlalchemy alembic asyncpg

# Run migrations
echo ""
echo "🔄 Running database migrations..."
cd "$SCRIPT_DIR"

# Set environment variables for Alembic
export DATABASE_URL="postgresql+asyncpg://$DB_USER:$DB_PASSWORD@$DB_HOST:$DB_PORT/$DB_NAME"

# Check if alembic is initialized
if [ ! -d "alembic" ]; then
    echo "   Initializing Alembic..."
    alembic init alembic
fi

# Run migrations
alembic upgrade head

# Seed initial data if in development
if [ "$ENVIRONMENT" == "development" ]; then
    echo ""
    echo "🌱 Seeding development data..."
    python3 << 'EOF'
import asyncio
import os
from sqlalchemy.ext.asyncio import create_async_engine, AsyncSession
from sqlalchemy.orm import sessionmaker
from models import Base, User, Game, SystemConfig

DATABASE_URL = os.getenv("DATABASE_URL")
engine = create_async_engine(DATABASE_URL)
async_session = sessionmaker(engine, class_=AsyncSession, expire_on_commit=False)

async def seed():
    async with async_session() as session:
        # Create test user
        test_user = User(
            id="user_test123",
            email="test@gamedatalabs.com",
            email_verified=True,
            display_name="Test User",
            status="ACTIVE",
            balance_usd=0.0,
            total_earned_usd=0.0
        )
        session.add(test_user)
        
        # Add some test games with validation
        games = [
            Game(id="game_001", exe_name="cs2.exe", title="Counter-Strike 2", genre="FPS", is_supported=True, demand_level=5),
            Game(id="game_002", exe_name="valorant.exe", title="Valorant", genre="FPS", is_supported=True, demand_level=5),
            Game(id="game_003", exe_name="genshinimpact.exe", title="Genshin Impact", genre="RPG", is_supported=True, demand_level=4),
        ]
        for game in games:
            game.validate()  # Validate constraints before adding
            session.add(game)
        
        await session.commit()
        print("   ✅ Development data seeded")

asyncio.run(seed())
EOF
fi

echo ""
echo "============================================================"
echo "✅ Database setup complete!"
echo "============================================================"
echo ""
echo "Database URL: postgresql+asyncpg://$DB_USER:$DB_PASSWORD@$DB_HOST:$DB_PORT/$DB_NAME"
echo ""
echo "Next steps:"
echo "1. Update your .env file with the database URL"
echo "2. Start the backend: python3 main.py"
echo ""
