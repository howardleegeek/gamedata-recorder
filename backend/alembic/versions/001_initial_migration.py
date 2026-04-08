"""Initial database migration.

Create all tables for GameData Labs backend.

Revision ID: 001
Revises:
Create Date: 2024-01-01 00:00:00.000000

"""

from typing import Sequence, Union

from alembic import op
import sqlalchemy as sa
from sqlalchemy.dialects import postgresql

# revision identifiers
revision: str = "001"
down_revision: Union[str, None] = None
branch_labels: Union[str, Sequence[str], None] = None
depends_on: Union[str, Sequence[str], None] = None


def upgrade() -> None:
    """Create all tables."""

    # Create users table
    op.create_table(
        "users",
        sa.Column("id", sa.String(32), primary_key=True),
        sa.Column("email", sa.String(255), unique=True, nullable=False, index=True),
        sa.Column("email_verified", sa.Boolean, default=False),
        sa.Column("display_name", sa.String(100), nullable=True),
        sa.Column("avatar_url", sa.String(500), nullable=True),
        sa.Column("password_hash", sa.String(255), nullable=True),
        sa.Column("provider", sa.String(50), default="email"),
        sa.Column("provider_id", sa.String(255), nullable=True),
        sa.Column(
            "status",
            sa.Enum(
                "ACTIVE",
                "INACTIVE",
                "SUSPENDED",
                "PENDING_VERIFICATION",
                name="userstatus",
            ),
            default="PENDING_VERIFICATION",
        ),
        sa.Column("is_admin", sa.Boolean, default=False),
        sa.Column("balance_usd", sa.Float, default=0.0),
        sa.Column("total_earned_usd", sa.Float, default=0.0),
        sa.Column("total_hours_recorded", sa.Float, default=0.0),
        sa.Column("created_at", sa.DateTime, default=sa.func.now()),
        sa.Column(
            "updated_at", sa.DateTime, default=sa.func.now(), onupdate=sa.func.now()
        ),
        sa.Column("last_login_at", sa.DateTime, nullable=True),
    )

    # Create user_sessions table
    op.create_table(
        "user_sessions",
        sa.Column("id", sa.String(32), primary_key=True),
        sa.Column(
            "user_id",
            sa.String(32),
            sa.ForeignKey("users.id", ondelete="CASCADE"),
            index=True,
        ),
        sa.Column("token", sa.String(255), unique=True, index=True),
        sa.Column("device_name", sa.String(100), nullable=True),
        sa.Column("device_type", sa.String(50), nullable=True),
        sa.Column("ip_address", sa.String(45), nullable=True),
        sa.Column("user_agent", sa.Text, nullable=True),
        sa.Column("created_at", sa.DateTime, default=sa.func.now()),
        sa.Column("expires_at", sa.DateTime, index=True),
        sa.Column("last_used_at", sa.DateTime, default=sa.func.now()),
        sa.Column("is_active", sa.Boolean, default=True),
    )

    # Create uploads table
    op.create_table(
        "uploads",
        sa.Column("id", sa.String(36), primary_key=True),
        sa.Column(
            "user_id",
            sa.String(32),
            sa.ForeignKey("users.id", ondelete="CASCADE"),
            index=True,
        ),
        sa.Column("game_control_id", sa.String(36), index=True),
        sa.Column("filename", sa.String(255), nullable=False),
        sa.Column("original_filename", sa.String(255), nullable=True),
        sa.Column("total_size_bytes", sa.Integer, nullable=False),
        sa.Column("chunk_size_bytes", sa.Integer, default=33554432),
        sa.Column("total_chunks", sa.Integer, nullable=False),
        sa.Column("game_exe", sa.String(255), nullable=True),
        sa.Column("game_title", sa.String(255), nullable=True),
        sa.Column("video_duration_seconds", sa.Float, nullable=True),
        sa.Column("video_width", sa.Integer, nullable=True),
        sa.Column("video_height", sa.Integer, nullable=True),
        sa.Column("video_codec", sa.String(50), nullable=True),
        sa.Column("video_fps", sa.Float, nullable=True),
        sa.Column("recorder_version", sa.String(50), nullable=True),
        sa.Column("hardware_id", sa.String(255), nullable=True),
        sa.Column(
            "status",
            sa.Enum(
                "IN_PROGRESS",
                "COMPLETED",
                "FAILED",
                "ABORTED",
                "SERVER_INVALID",
                name="uploadstatus",
            ),
            default="IN_PROGRESS",
        ),
        sa.Column("s3_key", sa.String(500), nullable=True),
        sa.Column("s3_upload_id", sa.String(255), nullable=True),
        sa.Column("local_path", sa.String(500), nullable=True),
        sa.Column("quality_score", sa.Float, nullable=True),
        sa.Column("earnings_usd", sa.Float, nullable=True),
        sa.Column("metadata", postgresql.JSONB, nullable=True),
        sa.Column("created_at", sa.DateTime, default=sa.func.now(), index=True),
        sa.Column("completed_at", sa.DateTime, nullable=True),
    )

    # Create indexes for uploads
    op.create_index("idx_uploads_user_status", "uploads", ["user_id", "status"])
    op.create_index("idx_uploads_created_at", "uploads", ["created_at"])

    # Create payouts table
    op.create_table(
        "payouts",
        sa.Column("id", sa.String(32), primary_key=True),
        sa.Column(
            "user_id",
            sa.String(32),
            sa.ForeignKey("users.id", ondelete="CASCADE"),
            index=True,
        ),
        sa.Column("amount_usd", sa.Float, nullable=False),
        sa.Column("fee_usd", sa.Float, default=0.0),
        sa.Column("net_amount_usd", sa.Float, nullable=False),
        sa.Column("method", sa.String(50), nullable=False),
        sa.Column("method_details", postgresql.JSONB, nullable=True),
        sa.Column(
            "status",
            sa.Enum(
                "PENDING",
                "PROCESSING",
                "COMPLETED",
                "FAILED",
                "CANCELLED",
                name="payoutstatus",
            ),
            default="PENDING",
        ),
        sa.Column("provider_transaction_id", sa.String(255), nullable=True),
        sa.Column("provider_response", postgresql.JSONB, nullable=True),
        sa.Column("created_at", sa.DateTime, default=sa.func.now()),
        sa.Column("processed_at", sa.DateTime, nullable=True),
        sa.Column("completed_at", sa.DateTime, nullable=True),
        sa.Column("reviewed_by", sa.String(32), nullable=True),
        sa.Column("notes", sa.Text, nullable=True),
    )

    # Create games table
    op.create_table(
        "games",
        sa.Column("id", sa.String(32), primary_key=True),
        sa.Column("exe_name", sa.String(100), unique=True, index=True),
        sa.Column("title", sa.String(255), nullable=False),
        sa.Column("genre", sa.String(100), nullable=True),
        sa.Column("developer", sa.String(255), nullable=True),
        sa.Column("release_year", sa.Integer, nullable=True),
        sa.Column("is_supported", sa.Boolean, default=True),
        sa.Column("is_unsupported", sa.Boolean, default=False),
        sa.Column("unsupported_reason", sa.Text, nullable=True),
        sa.Column("demand_level", sa.Integer, default=1),
        sa.Column("earnings_multiplier", sa.Float, default=1.0),
        sa.Column("metadata", postgresql.JSONB, nullable=True),
        sa.Column("created_at", sa.DateTime, default=sa.func.now()),
        sa.Column(
            "updated_at", sa.DateTime, default=sa.func.now(), onupdate=sa.func.now()
        ),
    )

    # Create system_config table
    op.create_table(
        "system_config",
        sa.Column("key", sa.String(100), primary_key=True),
        sa.Column("value", sa.Text, nullable=True),
        sa.Column("value_type", sa.String(20), default="string"),
        sa.Column("description", sa.Text, nullable=True),
        sa.Column(
            "updated_at", sa.DateTime, default=sa.func.now(), onupdate=sa.func.now()
        ),
        sa.Column("updated_by", sa.String(32), nullable=True),
    )

    # Create audit_logs table
    op.create_table(
        "audit_logs",
        sa.Column("id", sa.String(32), primary_key=True),
        sa.Column("user_id", sa.String(32), nullable=True, index=True),
        sa.Column("session_id", sa.String(32), nullable=True),
        sa.Column("ip_address", sa.String(45), nullable=True),
        sa.Column("user_agent", sa.Text, nullable=True),
        sa.Column("action", sa.String(100), nullable=False, index=True),
        sa.Column("resource_type", sa.String(50), nullable=True),
        sa.Column("resource_id", sa.String(32), nullable=True),
        sa.Column("details", postgresql.JSONB, nullable=True),
        sa.Column("status", sa.String(20), default="success"),
        sa.Column("error_message", sa.Text, nullable=True),
        sa.Column("created_at", sa.DateTime, default=sa.func.now(), index=True),
    )

    # Create indexes for audit_logs
    op.create_index(
        "idx_audit_logs_action_time", "audit_logs", ["action", "created_at"]
    )
    op.create_index("idx_audit_logs_user_action", "audit_logs", ["user_id", "action"])

    # Insert default system config
    op.bulk_insert(
        "system_config",
        [
            {
                "key": "min_payout_amount",
                "value": "10.00",
                "value_type": "float",
                "description": "Minimum payout amount in USD",
            },
            {
                "key": "payout_fee_percent",
                "value": "2.9",
                "value_type": "float",
                "description": "Payout processing fee percentage",
            },
            {
                "key": "default_earnings_per_hour",
                "value": "0.50",
                "value_type": "float",
                "description": "Default earnings per hour of recording",
            },
            {
                "key": "maintenance_mode",
                "value": "false",
                "value_type": "bool",
                "description": "Enable maintenance mode",
            },
        ],
    )


def downgrade() -> None:
    """Drop all tables."""
    op.drop_table("audit_logs")
    op.drop_table("system_config")
    op.drop_table("games")
    op.drop_table("payouts")
    op.drop_table("uploads")
    op.drop_table("user_sessions")
    op.drop_table("users")

    # Drop enum types
    op.execute("DROP TYPE IF EXISTS userstatus")
    op.execute("DROP TYPE IF EXISTS uploadstatus")
    op.execute("DROP TYPE IF EXISTS payoutstatus")
