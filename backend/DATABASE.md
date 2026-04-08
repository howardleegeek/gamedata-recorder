# GameData Labs - Database Documentation

## Overview

GameData Labs backend uses **PostgreSQL** with **SQLAlchemy 2.0** async ORM.

- **Database**: PostgreSQL 14+
- **ORM**: SQLAlchemy 2.0 with asyncpg driver
- **Migrations**: Alembic
- **Connection**: Async connection pooling

## Quick Start

### 1. Install PostgreSQL

**macOS:**
```bash
brew install postgresql
brew services start postgresql
```

**Ubuntu:**
```bash
sudo apt-get install postgresql postgresql-contrib
sudo systemctl start postgresql
```

### 2. Run Setup Script

```bash
cd backend
chmod +x setup_database.sh
./setup_database.sh development
```

This will:
- Create database `gamedata`
- Create user `gamedata`
- Run all migrations
- Seed development data (if in dev mode)

### 3. Configure Environment

Add to your `.env` file:
```bash
# Database Configuration
DATABASE_URL=postgresql+asyncpg://gamedata:gamedata@localhost:5432/gamedata
# Or use individual components:
DB_HOST=localhost
DB_PORT=5432
DB_NAME=gamedata
DB_USER=gamedata
DB_PASSWORD=gamedata
```

### 4. Verify Connection

```bash
python3 -c "
import asyncio
from sqlalchemy.ext.asyncio import create_async_engine

async def test():
    engine = create_async_engine('postgresql+asyncpg://gamedata:gamedata@localhost:5432/gamedata')
    async with engine.connect() as conn:
        result = await conn.execute('SELECT version()')
        print(result.scalar())

asyncio.run(test())
"
```

## Database Schema

### Tables

#### users
Main user accounts table.

| Column | Type | Description |
|--------|------|-------------|
| id | VARCHAR(32) | Primary key |
| email | VARCHAR(255) | Unique email address |
| email_verified | BOOLEAN | Email verification status |
| display_name | VARCHAR(100) | User display name |
| password_hash | VARCHAR(255) | Bcrypt password hash |
| provider | VARCHAR(50) | Auth provider (email/google/discord) |
| status | ENUM | Account status (active/inactive/suspended/pending) |
| balance_usd | FLOAT | Current balance |
| total_earned_usd | FLOAT | Lifetime earnings |
| total_hours_recorded | FLOAT | Total recording hours |
| created_at | TIMESTAMP | Account creation time |

#### uploads
Recording uploads table.

| Column | Type | Description |
|--------|------|-------------|
| id | UUID | Primary key |
| user_id | VARCHAR(32) | Foreign key to users |
| filename | VARCHAR(255) | Stored filename |
| total_size_bytes | INTEGER | File size |
| status | ENUM | Upload status |
| game_exe | VARCHAR(255) | Game executable name |
| video_duration_seconds | FLOAT | Recording duration |
| earnings_usd | FLOAT | Calculated earnings |
| metadata | JSONB | Additional metadata |
| created_at | TIMESTAMP | Upload time |

#### payouts
User payout requests.

| Column | Type | Description |
|--------|------|-------------|
| id | VARCHAR(32) | Primary key |
| user_id | VARCHAR(32) | Foreign key to users |
| amount_usd | FLOAT | Requested amount |
| method | VARCHAR(50) | Payment method |
| status | ENUM | Payout status |
| provider_transaction_id | VARCHAR(255) | Payment provider ID |
| created_at | TIMESTAMP | Request time |

#### games
Supported games catalog.

| Column | Type | Description |
|--------|------|-------------|
| id | VARCHAR(32) | Primary key |
| exe_name | VARCHAR(100) | Game executable name |
| title | VARCHAR(255) | Game title |
| genre | VARCHAR(100) | Game genre |
| is_supported | BOOLEAN | Whether game is supported |
| demand_level | INTEGER | Demand level (1-5) |
| earnings_multiplier | FLOAT | Earnings multiplier |

#### audit_logs
Audit trail for important actions.

| Column | Type | Description |
|--------|------|-------------|
| id | VARCHAR(32) | Primary key |
| user_id | VARCHAR(32) | User who performed action |
| action | VARCHAR(100) | Action type |
| resource_type | VARCHAR(50) | Type of resource affected |
| details | JSONB | Action details |
| created_at | TIMESTAMP | Action time |

## Migrations

### Create New Migration

```bash
cd backend
alembic revision --autogenerate -m "description of changes"
```

### Run Migrations

```bash
# Upgrade to latest
alembic upgrade head

# Upgrade to specific version
alembic upgrade 001

# Downgrade
alembic downgrade -1
```

### Migration Status

```bash
alembic current    # Show current version
alembic history    # Show all migrations
alembic heads      # Show latest migrations
```

## Common Operations

### Backup Database

```bash
pg_dump -h localhost -U gamedata gamedata > backup_$(date +%Y%m%d).sql
```

### Restore Database

```bash
psql -h localhost -U gamedata gamedata < backup_20240101.sql
```

### Reset Database (Development)

```bash
./setup_database.sh development
```

## Performance Optimization

### Indexes

The following indexes are created automatically:

- `users.email` - Unique lookup
- `uploads.user_id + status` - User upload queries
- `uploads.created_at` - Time-based queries
- `audit_logs.action + created_at` - Audit queries

### Connection Pooling

Default configuration:
- Pool size: 10 connections
- Max overflow: 20 connections
- Pool pre-ping: Enabled (verifies connections before use)

### Query Optimization Tips

1. Use `selectin` loading for relationships
2. Add indexes for frequently queried columns
3. Use `EXPLAIN ANALYZE` to check query plans
4. Consider partitioning for large tables (uploads, audit_logs)

## Troubleshooting

### Connection Refused

```bash
# Check PostgreSQL is running
sudo systemctl status postgresql  # Linux
brew services list | grep postgresql  # macOS

# Start PostgreSQL
sudo systemctl start postgresql  # Linux
brew services start postgresql  # macOS
```

### Permission Denied

```bash
# Grant privileges
sudo -u postgres psql -c "GRANT ALL PRIVILEGES ON DATABASE gamedata TO gamedata;"
```

### Migration Failed

```bash
# Check current version
alembic current

# Mark as applied (if manually fixed)
alembic stamp head

# Or reset and re-run
alembic downgrade base
alembic upgrade head
```

## Environment-Specific Notes

### Development
- Auto-seeds test data
- Logging enabled
- Pool size: 10

### Staging
- Mirrors production
- Reduced pool size: 5
- Regular backups

### Production
- Connection pooling: 20-50
- Read replicas recommended
- Automated backups (daily)
- Monitoring required

## Security

1. **Never commit database credentials**
2. Use environment variables
3. Enable SSL in production
4. Restrict database access by IP
5. Regular security updates
6. Audit log all sensitive operations

## Support

For database issues:
1. Check logs: `tail -f /var/log/postgresql/*.log`
2. Review migrations: `alembic history`
3. Test connection: `psql -h localhost -U gamedata -d gamedata`
