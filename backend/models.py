"""
GameData Labs - Database Models
SQLAlchemy async ORM models for PostgreSQL
"""

from datetime import datetime
from typing import Optional, List
from enum import Enum as PyEnum

from sqlalchemy import (
    String,
    Integer,
    Float,
    DateTime,
    Boolean,
    Text,
    ForeignKey,
    Index,
    Enum,
    create_engine,
)
from sqlalchemy.orm import DeclarativeBase, Mapped, mapped_column, relationship
from sqlalchemy.dialects.postgresql import UUID, JSONB
import uuid


class Base(DeclarativeBase):
    """Base class for all models."""

    pass


class UserStatus(str, PyEnum):
    """User account status."""

    ACTIVE = "active"
    INACTIVE = "inactive"
    SUSPENDED = "suspended"
    PENDING_VERIFICATION = "pending_verification"


class UploadStatus(str, PyEnum):
    """Upload processing status."""

    IN_PROGRESS = "in_progress"
    COMPLETED = "completed"
    FAILED = "failed"
    ABORTED = "aborted"
    SERVER_INVALID = "server_invalid"


class PayoutStatus(str, PyEnum):
    """Payout request status."""

    PENDING = "pending"
    PROCESSING = "processing"
    COMPLETED = "completed"
    FAILED = "failed"
    CANCELLED = "cancelled"


class User(Base):
    """User account model."""

    __tablename__ = "users"

    id: Mapped[str] = mapped_column(String(32), primary_key=True)
    email: Mapped[str] = mapped_column(
        String(255), unique=True, index=True, nullable=False
    )
    email_verified: Mapped[bool] = mapped_column(Boolean, default=False)

    # Profile
    display_name: Mapped[Optional[str]] = mapped_column(String(100), nullable=True)
    avatar_url: Mapped[Optional[str]] = mapped_column(String(500), nullable=True)

    # Auth
    password_hash: Mapped[Optional[str]] = mapped_column(String(255), nullable=True)
    provider: Mapped[str] = mapped_column(
        String(50), default="email"
    )  # email, google, discord
    provider_id: Mapped[Optional[str]] = mapped_column(String(255), nullable=True)

    # Status
    status: Mapped[UserStatus] = mapped_column(
        Enum(UserStatus), default=UserStatus.PENDING_VERIFICATION
    )
    is_admin: Mapped[bool] = mapped_column(Boolean, default=False)

    # Financial
    balance_usd: Mapped[float] = mapped_column(Float, default=0.0)
    total_earned_usd: Mapped[float] = mapped_column(Float, default=0.0)
    total_hours_recorded: Mapped[float] = mapped_column(Float, default=0.0)

    # Metadata
    created_at: Mapped[datetime] = mapped_column(DateTime, default=datetime.utcnow)
    updated_at: Mapped[datetime] = mapped_column(
        DateTime, default=datetime.utcnow, onupdate=datetime.utcnow
    )
    last_login_at: Mapped[Optional[datetime]] = mapped_column(DateTime, nullable=True)

    # Relationships
    uploads: Mapped[List["Upload"]] = relationship(
        "Upload", back_populates="user", lazy="selectin", cascade="all, delete-orphan"
    )
    payouts: Mapped[List["Payout"]] = relationship(
        "Payout", back_populates="user", lazy="selectin"
    )
    sessions: Mapped[List["UserSession"]] = relationship(
        "UserSession",
        back_populates="user",
        lazy="selectin",
        cascade="all, delete-orphan",
    )

    def __repr__(self) -> str:
        return f"<User(id={self.id}, email={self.email}, status={self.status})>"


class UserSession(Base):
    """User login session for multi-device support."""

    __tablename__ = "user_sessions"

    id: Mapped[str] = mapped_column(String(32), primary_key=True)
    user_id: Mapped[str] = mapped_column(
        ForeignKey("users.id", ondelete="CASCADE"), index=True
    )

    # Session info
    token: Mapped[str] = mapped_column(String(255), unique=True, index=True)
    device_name: Mapped[Optional[str]] = mapped_column(String(100), nullable=True)
    device_type: Mapped[Optional[str]] = mapped_column(
        String(50), nullable=True
    )  # desktop, mobile
    ip_address: Mapped[Optional[str]] = mapped_column(String(45), nullable=True)
    user_agent: Mapped[Optional[str]] = mapped_column(Text, nullable=True)

    # Timestamps
    created_at: Mapped[datetime] = mapped_column(DateTime, default=datetime.utcnow)
    expires_at: Mapped[datetime] = mapped_column(DateTime, index=True)
    last_used_at: Mapped[datetime] = mapped_column(DateTime, default=datetime.utcnow)

    # Status
    is_active: Mapped[bool] = mapped_column(Boolean, default=True)

    # Relationship
    user: Mapped["User"] = relationship("User", back_populates="sessions")

    def __repr__(self) -> str:
        return f"<UserSession(id={self.id}, user_id={self.user_id}, device={self.device_name})>"


class Upload(Base):
    """Recording upload model."""

    __tablename__ = "uploads"

    id: Mapped[str] = mapped_column(String(36), primary_key=True)  # UUID
    user_id: Mapped[str] = mapped_column(
        ForeignKey("users.id", ondelete="CASCADE"), index=True
    )
    game_control_id: Mapped[str] = mapped_column(String(36), index=True)

    # File info
    filename: Mapped[str] = mapped_column(String(255))
    original_filename: Mapped[Optional[str]] = mapped_column(String(255), nullable=True)
    total_size_bytes: Mapped[int] = mapped_column(Integer)
    chunk_size_bytes: Mapped[int] = mapped_column(Integer, default=33554432)  # 32MB
    total_chunks: Mapped[int] = mapped_column(Integer)

    # Game info
    game_exe: Mapped[Optional[str]] = mapped_column(String(255), nullable=True)
    game_title: Mapped[Optional[str]] = mapped_column(String(255), nullable=True)

    # Video metadata
    video_duration_seconds: Mapped[Optional[float]] = mapped_column(
        Float, nullable=True
    )
    video_width: Mapped[Optional[int]] = mapped_column(Integer, nullable=True)
    video_height: Mapped[Optional[int]] = mapped_column(Integer, nullable=True)
    video_codec: Mapped[Optional[str]] = mapped_column(String(50), nullable=True)
    video_fps: Mapped[Optional[float]] = mapped_column(Float, nullable=True)

    # Technical metadata
    recorder_version: Mapped[Optional[str]] = mapped_column(String(50), nullable=True)
    hardware_id: Mapped[Optional[str]] = mapped_column(String(255), nullable=True)

    # Status
    status: Mapped[UploadStatus] = mapped_column(
        Enum(UploadStatus), default=UploadStatus.IN_PROGRESS
    )

    # Storage
    s3_key: Mapped[Optional[str]] = mapped_column(String(500), nullable=True)
    s3_upload_id: Mapped[Optional[str]] = mapped_column(String(255), nullable=True)
    local_path: Mapped[Optional[str]] = mapped_column(String(500), nullable=True)

    # Quality & earnings
    quality_score: Mapped[Optional[float]] = mapped_column(Float, nullable=True)
    earnings_usd: Mapped[Optional[float]] = mapped_column(Float, nullable=True)

    # Additional metadata (flexible JSON)
    upload_metadata: Mapped[Optional[dict]] = mapped_column(
        "metadata", JSONB, nullable=True
    )

    # Timestamps
    created_at: Mapped[datetime] = mapped_column(
        DateTime, default=datetime.utcnow, index=True
    )
    completed_at: Mapped[Optional[datetime]] = mapped_column(DateTime, nullable=True)

    # Indexes for common queries
    __table_args__ = (
        Index("idx_uploads_user_status", "user_id", "status"),
        Index("idx_uploads_created_at", "created_at"),
    )

    # Relationship
    user: Mapped["User"] = relationship("User", back_populates="uploads")

    def __repr__(self) -> str:
        return f"<Upload(id={self.id}, user_id={self.user_id}, status={self.status})>"


class Payout(Base):
    """User payout request model."""

    __tablename__ = "payouts"

    id: Mapped[str] = mapped_column(String(32), primary_key=True)
    user_id: Mapped[str] = mapped_column(
        ForeignKey("users.id", ondelete="CASCADE"), index=True
    )

    # Amount
    amount_usd: Mapped[float] = mapped_column(Float)
    fee_usd: Mapped[float] = mapped_column(Float, default=0.0)
    net_amount_usd: Mapped[float] = mapped_column(Float)

    # Method
    method: Mapped[str] = mapped_column(String(50))  # paypal, stripe, bank_transfer
    method_details: Mapped[Optional[dict]] = mapped_column(JSONB, nullable=True)

    # Status
    status: Mapped[PayoutStatus] = mapped_column(
        Enum(PayoutStatus), default=PayoutStatus.PENDING
    )

    # Provider info
    provider_transaction_id: Mapped[Optional[str]] = mapped_column(
        String(255), nullable=True
    )
    provider_response: Mapped[Optional[dict]] = mapped_column(JSONB, nullable=True)

    # Timestamps
    created_at: Mapped[datetime] = mapped_column(DateTime, default=datetime.utcnow)
    processed_at: Mapped[Optional[datetime]] = mapped_column(DateTime, nullable=True)
    completed_at: Mapped[Optional[datetime]] = mapped_column(DateTime, nullable=True)

    # Admin
    reviewed_by: Mapped[Optional[str]] = mapped_column(String(32), nullable=True)
    notes: Mapped[Optional[str]] = mapped_column(Text, nullable=True)

    # Relationship
    user: Mapped["User"] = relationship("User", back_populates="payouts")

    def __repr__(self) -> str:
        return f"<Payout(id={self.id}, user_id={self.user_id}, amount={self.amount_usd}, status={self.status})>"


class Game(Base):
    """Supported games catalog."""

    __tablename__ = "games"

    id: Mapped[str] = mapped_column(String(32), primary_key=True)
    exe_name: Mapped[str] = mapped_column(String(100), unique=True, index=True)

    # Info
    title: Mapped[str] = mapped_column(String(255))
    genre: Mapped[Optional[str]] = mapped_column(String(100), nullable=True)
    developer: Mapped[Optional[str]] = mapped_column(String(255), nullable=True)
    release_year: Mapped[Optional[int]] = mapped_column(Integer, nullable=True)

    # Status
    is_supported: Mapped[bool] = mapped_column(Boolean, default=True)
    is_unsupported: Mapped[bool] = mapped_column(Boolean, default=False)
    unsupported_reason: Mapped[Optional[str]] = mapped_column(Text, nullable=True)

    # Demand & pricing
    demand_level: Mapped[int] = mapped_column(Integer, default=1)  # 1-5
    earnings_multiplier: Mapped[float] = mapped_column(Float, default=1.0)

    # Metadata
    game_metadata: Mapped[Optional[dict]] = mapped_column(
        "metadata", JSONB, nullable=True
    )

    # Timestamps
    created_at: Mapped[datetime] = mapped_column(DateTime, default=datetime.utcnow)
    updated_at: Mapped[datetime] = mapped_column(
        DateTime, default=datetime.utcnow, onupdate=datetime.utcnow
    )

    def __repr__(self) -> str:
        return (
            f"<Game(id={self.id}, title={self.title}, supported={self.is_supported})>"
        )


class SystemConfig(Base):
    """System-wide configuration."""

    __tablename__ = "system_config"

    key: Mapped[str] = mapped_column(String(100), primary_key=True)
    value: Mapped[Optional[str]] = mapped_column(Text, nullable=True)
    value_type: Mapped[str] = mapped_column(
        String(20), default="string"
    )  # string, int, float, json
    description: Mapped[Optional[str]] = mapped_column(Text, nullable=True)

    updated_at: Mapped[datetime] = mapped_column(
        DateTime, default=datetime.utcnow, onupdate=datetime.utcnow
    )
    updated_by: Mapped[Optional[str]] = mapped_column(String(32), nullable=True)

    def __repr__(self) -> str:
        return f"<SystemConfig(key={self.key}, value={self.value})>"


class AuditLog(Base):
    """Audit log for important actions."""

    __tablename__ = "audit_logs"

    id: Mapped[str] = mapped_column(String(32), primary_key=True)

    # Who
    user_id: Mapped[Optional[str]] = mapped_column(
        String(32), nullable=True, index=True
    )
    session_id: Mapped[Optional[str]] = mapped_column(String(32), nullable=True)
    ip_address: Mapped[Optional[str]] = mapped_column(String(45), nullable=True)
    user_agent: Mapped[Optional[str]] = mapped_column(Text, nullable=True)

    # What
    action: Mapped[str] = mapped_column(
        String(100), index=True
    )  # login, upload, payout_request, etc.
    resource_type: Mapped[Optional[str]] = mapped_column(
        String(50), nullable=True
    )  # user, upload, payout
    resource_id: Mapped[Optional[str]] = mapped_column(String(32), nullable=True)

    # Details
    details: Mapped[Optional[dict]] = mapped_column(JSONB, nullable=True)
    status: Mapped[str] = mapped_column(
        String(20), default="success"
    )  # success, failure
    error_message: Mapped[Optional[str]] = mapped_column(Text, nullable=True)

    # When
    created_at: Mapped[datetime] = mapped_column(
        DateTime, default=datetime.utcnow, index=True
    )

    # Indexes
    __table_args__ = (
        Index("idx_audit_logs_action_time", "action", "created_at"),
        Index("idx_audit_logs_user_action", "user_id", "action"),
    )

    def __repr__(self) -> str:
        return f"<AuditLog(id={self.id}, action={self.action}, user_id={self.user_id})>"


# Database connection helper
from sqlalchemy.ext.asyncio import create_async_engine, AsyncSession, async_sessionmaker


# Async engine factory
def create_async_db_engine(database_url: str):
    """Create async database engine."""
    return create_async_engine(
        database_url,
        echo=False,  # Set to True for SQL logging
        pool_size=10,
        max_overflow=20,
        pool_pre_ping=True,  # Verify connections before use
    )


# Session factory
def create_async_session_factory(engine):
    """Create async session factory."""
    return async_sessionmaker(
        engine,
        class_=AsyncSession,
        expire_on_commit=False,
        autoflush=False,
    )


# Sync engine for migrations (Alembic)
def create_sync_db_engine(database_url: str):
    """Create sync database engine for migrations."""
    # Convert asyncpg URL to psycopg2 URL
    sync_url = database_url.replace("postgresql+asyncpg://", "postgresql://")
    return create_engine(sync_url)
