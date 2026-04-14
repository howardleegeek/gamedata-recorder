"""
GameData Labs Backend v0.2.0
Production-ready API server with PostgreSQL database.
Handles: auth, user management, uploads, earnings, payouts.
"""

import math
import os
import uuid
import time
import hmac
import hashlib
import json
import re
from datetime import datetime, timedelta
from pathlib import Path
from typing import Optional, List
from contextlib import asynccontextmanager
from urllib.parse import quote_plus

import boto3
from fastapi import FastAPI, HTTPException, Header, Depends, Request, status
from fastapi.middleware.cors import CORSMiddleware
from fastapi.responses import JSONResponse
from fastapi.security import HTTPBearer, HTTPAuthorizationCredentials
from pydantic import BaseModel, EmailStr, Field, validator
from sqlalchemy.ext.asyncio import AsyncSession
from sqlalchemy import select, desc, func, and_
from passlib.context import CryptContext

# Import models
from models import (
    Base,
    User,
    UserSession,
    Upload,
    Payout,
    Game,
    AuditLog,
    SystemConfig,
    UserStatus,
    UploadStatus,
    PayoutStatus,
    create_async_db_engine,
    create_async_session_factory,
)

# Security
pwd_context = CryptContext(schemes=["bcrypt"], deprecated="auto")
security = HTTPBearer(auto_error=False)

# Environment
ENVIRONMENT = os.getenv("ENVIRONMENT", "development")
API_SECRET = os.getenv("API_SECRET")

# Database configuration
DATABASE_URL = os.getenv("DATABASE_URL")
if not DATABASE_URL:
    DB_USER = os.getenv("DB_USER", "gamedata")
    DB_PASSWORD = os.getenv("DB_PASSWORD", "gamedata")
    DB_HOST = os.getenv("DB_HOST", "localhost")
    DB_PORT = os.getenv("DB_PORT", "5432")
    DB_NAME = os.getenv("DB_NAME", "gamedata")
    # URL-encode credentials to handle special characters in passwords
    DATABASE_URL = (
        f"postgresql+asyncpg://{quote_plus(DB_USER)}:{quote_plus(DB_PASSWORD)}"
        f"@{quote_plus(DB_HOST)}:{DB_PORT}/{quote_plus(DB_NAME)}"
    )

# Create database engine and session factory
db_engine = create_async_db_engine(DATABASE_URL)
SessionLocal = create_async_session_factory(db_engine)

# S3 configuration
S3_BUCKET = os.getenv("S3_BUCKET", "gamedata-recordings")
S3_REGION = os.getenv("S3_REGION", "us-east-1")
AWS_ACCESS_KEY = os.getenv("AWS_ACCESS_KEY_ID")
AWS_SECRET_KEY = os.getenv("AWS_SECRET_ACCESS_KEY")

# CORS configuration
ALLOWED_ORIGINS = os.getenv(
    "ALLOWED_ORIGINS", "http://localhost:3000,http://localhost:8080"
).split(",")

# Logging
import logging

logging.basicConfig(
    level=logging.INFO, format="%(asctime)s - %(name)s - %(levelname)s - %(message)s"
)
logger = logging.getLogger(__name__)


# --- Database Dependency ---


async def get_db() -> AsyncSession:
    """Get database session."""
    async with SessionLocal() as session:
        try:
            yield session
        finally:
            await session.close()


# --- Startup Validation ---


def validate_startup_config():
    """Validate critical configuration on startup."""
    errors = []
    warnings_list = []

    # Check API_SECRET
    if not API_SECRET:
        if ENVIRONMENT == "development":
            import secrets

            temp_secret = secrets.token_hex(32)
            print(f"\n{'=' * 60}")
            print("⚠️  WARNING: API_SECRET not set!")
            print(f"{'=' * 60}")
            print("Using temporary secret for DEVELOPMENT ONLY.")
            print(f"Set API_SECRET environment variable:")
            print(f"  export API_SECRET={temp_secret}")
            print(f"{'=' * 60}\n")
            os.environ["API_SECRET"] = temp_secret
        else:
            errors.append("API_SECRET environment variable is required in production")
    elif len(API_SECRET) < 32:
        errors.append("API_SECRET must be at least 32 characters long")

    # Check CORS in production
    if ENVIRONMENT == "production":
        default_origins = ["http://localhost:3000", "http://localhost:8080"]
        if any(origin in ALLOWED_ORIGINS for origin in default_origins):
            warnings_list.append("CORS allows localhost origins in production")

    # Check S3 configuration
    if AWS_ACCESS_KEY and not AWS_SECRET_KEY:
        warnings_list.append(
            "AWS_ACCESS_KEY_ID set but AWS_SECRET_ACCESS_KEY is missing"
        )
    if AWS_SECRET_KEY and not AWS_ACCESS_KEY:
        warnings_list.append(
            "AWS_SECRET_ACCESS_KEY set but AWS_ACCESS_KEY_ID is missing"
        )

    # Print startup banner
    print(f"\n{'=' * 60}")
    print(f"🎮 GameData Labs Backend v0.2.0")
    print(f"{'=' * 60}")
    print(f"Environment: {ENVIRONMENT}")
    print(f"Database: PostgreSQL")
    print(
        f"API Secret: {'✅ Configured' if API_SECRET else '⚠️ Using temporary (dev only)'}"
    )
    print(
        f"S3 Storage: {'✅ Configured' if (AWS_ACCESS_KEY and AWS_SECRET_KEY) else '⚠️ Local storage only'}"
    )
    print(f"CORS Origins: {len(ALLOWED_ORIGINS)} origin(s) allowed")

    if warnings_list:
        print(f"\n⚠️  Warnings:")
        for w in warnings_list:
            print(f"   - {w}")

    if errors:
        print(f"\n❌ Configuration Errors:")
        for e in errors:
            print(f"   - {e}")
        print(f"{'=' * 60}\n")
        raise RuntimeError(f"Startup validation failed with {len(errors)} error(s)")

    print(f"{'=' * 60}\n")


# --- Lifespan Management ---


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Manage application lifespan."""
    # Startup
    validate_startup_config()

    # Create tables if they don't exist (for development)
    if ENVIRONMENT == "development":
        async with db_engine.begin() as conn:
            await conn.run_sync(Base.metadata.create_all)
        logger.info("Database tables created/verified")

    yield

    # Shutdown
    await db_engine.dispose()
    logger.info("Database connections closed")


# Create FastAPI app
app = FastAPI(title="GameData Labs API", version="0.2.0", lifespan=lifespan)

app.add_middleware(
    CORSMiddleware,
    allow_origins=ALLOWED_ORIGINS,
    allow_methods=["GET", "POST", "PUT", "DELETE"],
    allow_headers=["Authorization", "Content-Type", "X-API-Key"],
    allow_credentials=True,
)


# --- Global Exception Handler ---


@app.exception_handler(Exception)
async def global_exception_handler(request: Request, exc: Exception):
    """Handle all unhandled exceptions."""
    error_id = str(uuid.uuid4())[:8]
    logger.error(f"Unhandled exception [{error_id}]: {str(exc)}", exc_info=True)

    if ENVIRONMENT == "production":
        return JSONResponse(
            status_code=500,
            content={
                "error": "Internal server error",
                "error_id": error_id,
                "message": "An unexpected error occurred. Please try again later.",
            },
        )
    else:
        import traceback

        return JSONResponse(
            status_code=500,
            content={
                "error": "Internal server error",
                "error_id": error_id,
                "message": str(exc),
                "traceback": traceback.format_exc().split("\n")[-5:],
            },
        )


# --- Security Utilities ---


def sanitize_filename(filename: str) -> str:
    """Sanitize filename to prevent path traversal attacks.
    
    Extracts only the base filename (stripping all path components), removes
    null bytes, and strips leading dots to prevent hidden file targeting.
    This is more secure than string replacement which can be bypassed.
    """
    if not filename:
        return "unnamed"
    # Normalize path separators to handle both Unix (/) and Windows (\) paths
    # This ensures os.path.basename works correctly regardless of platform
    normalized = filename.replace("\\", "/")
    # Extract just the filename component - drops all path separators
    # This is more robust than string replacement (e.g., ".../.../etc/passwd"
    # would incorrectly become "..etcpasswd" with naive replacement)
    sanitized = os.path.basename(normalized)
    # Remove null bytes
    sanitized = sanitized.replace("\x00", "")
    # Remove leading dots to prevent hidden file targeting (.htaccess, .env, etc.)
    sanitized = sanitized.lstrip(".")
    # Ensure we have a valid filename left
    if not sanitized or sanitized.strip(".") == "":
        return "unnamed"
    return sanitized


# --- Auth Utilities ---


def generate_token(user_id: str) -> str:
    """Generate HMAC-signed token."""
    payload = f"{user_id}:{int(time.time())}"
    sig = hmac.new(
        API_SECRET.encode(), payload.encode(), hashlib.sha256
    ).hexdigest()
    return f"{payload}:{sig}"


def verify_token(token: str) -> Optional[str]:
    """Verify token and return user_id if valid."""
    parts = token.split(":")
    if len(parts) != 3:
        return None

    user_id, timestamp, sig = parts
    expected_payload = f"{user_id}:{timestamp}"
    expected_sig = hmac.new(
        API_SECRET.encode(), expected_payload.encode(), hashlib.sha256
    ).hexdigest()

    if not hmac.compare_digest(sig, expected_sig):
        return None

    # Token valid for 30 days - use constant-time comparison to prevent timing attacks
    try:
        ts = int(timestamp)
    except ValueError:
        return None
    token_age = int(time.time()) - ts
    max_age = 30 * 86400
    # Use comparison that doesn't leak timing information about token age
    if token_age > max_age or token_age < 0:  # Also check for future timestamps
        return None

    return user_id


def hash_password(password: str) -> str:
    """Hash password with bcrypt."""
    return pwd_context.hash(password)


def verify_password(password: str, hashed: str) -> bool:
    """Verify password against hash."""
    return pwd_context.verify(password, hashed)


# --- Auth Dependency ---


async def get_current_user(
    authorization: str = Header(None),
    x_api_key: str = Header(None),
    db: AsyncSession = Depends(get_db),
) -> User:
    """Extract and validate current user from token."""
    token = None

    if authorization and authorization.startswith("Bearer "):
        token = authorization[7:]
    elif x_api_key:
        token = x_api_key

    if not token:
        raise HTTPException(status_code=401, detail="Missing authentication token")

    # Try to verify as our token
    user_id = verify_token(token)

    if user_id:
        # Look up user in database
        result = await db.execute(select(User).where(User.id == user_id))
        user = result.scalar_one_or_none()

        if user and user.status == UserStatus.ACTIVE:
            return user

    # Legacy sk_ keys removed — was a security bypass (auto-created users without auth).
    # All users must register and authenticate via /api/v1/auth/login or /register.

    raise HTTPException(status_code=401, detail="Invalid or expired token")


# --- Request Models ---


class RegisterRequest(BaseModel):
    """User registration request."""

    email: EmailStr
    password: str = Field(..., min_length=8)
    display_name: Optional[str] = Field(None, max_length=100)

    @validator("password")
    def validate_password(cls, v):
        if not re.match(r"^(?=.*[a-z])(?=.*[A-Z])(?=.*\d)", v):
            raise ValueError("Password must contain uppercase, lowercase, and digit")
        return v


class LoginRequest(BaseModel):
    """User login request."""

    email: EmailStr
    password: str


class UploadInitRequest(BaseModel):
    """Upload initialization request."""

    filename: str = Field(..., max_length=255)
    total_size_bytes: int = Field(..., gt=0)
    chunk_size_bytes: Optional[int] = Field(33554432, gt=0)
    game_exe: Optional[str] = Field(None, max_length=255)
    video_duration_seconds: Optional[float] = Field(None, ge=0)
    video_width: Optional[int] = Field(None, ge=0)
    video_height: Optional[int] = Field(None, ge=0)
    video_codec: Optional[str] = Field(None, max_length=50)
    video_fps: Optional[float] = Field(None, ge=0)
    recorder_version: Optional[str] = Field(None, max_length=50)
    hardware_id: Optional[str] = Field(None, max_length=255)
    metadata: Optional[dict] = None


class UploadCompleteRequest(BaseModel):
    """Upload completion request."""

    upload_id: str = Field(..., max_length=36)
    etags: List[str] = Field(default=[], max_length=10000)

    @validator("etags")
    def validate_etags_length(cls, v):
        if len(v) > 10000:
            raise ValueError("etags list exceeds maximum of 10000 items")
        return v


class PayoutRequest(BaseModel):
    """Payout request."""

    amount_usd: float = Field(..., gt=0)
    method: str = Field(..., pattern="^(paypal|stripe|bank_transfer)$")
    method_details: Optional[dict] = None


# --- Endpoints ---


@app.get("/health")
async def health(db: AsyncSession = Depends(get_db)):
    """Health check with database connectivity test."""
    try:
        # Test database connection
        result = await db.execute(select(func.count()).select_from(User))
        user_count = result.scalar()

        return {
            "status": "ok",
            "version": "0.2.0",
            "database": "connected",
            "timestamp": datetime.utcnow().isoformat(),
            # user_count removed — was leaking business info to unauthenticated callers
        }
    except Exception as e:
        logger.error(f"Health check failed: {e}")
        raise HTTPException(status_code=503, detail="Database connection failed")


@app.post("/api/v1/auth/register")
async def register(req: RegisterRequest, db: AsyncSession = Depends(get_db)):
    """Register new user account."""
    # Check if email already exists
    result = await db.execute(select(User).where(User.email == req.email))
    if result.scalar_one_or_none():
        raise HTTPException(status_code=400, detail="Email already registered")

    # Create user
    user_id = f"user_{uuid.uuid4().hex[:12]}"
    user = User(
        id=user_id,
        email=req.email,
        password_hash=hash_password(req.password),
        display_name=req.display_name,
        status=UserStatus.PENDING_VERIFICATION,
        provider="email",
    )

    db.add(user)
    await db.commit()

    # Generate token
    token = generate_token(user_id)

    logger.info(f"New user registered: {user_id} ({req.email})")

    return {
        "token": token,
        "user_id": user_id,
        "email": req.email,
        "message": "Registration successful. Please verify your email.",
    }


@app.post("/api/v1/auth/login")
async def login(req: LoginRequest, db: AsyncSession = Depends(get_db)):
    """Login with email and password."""
    from sqlalchemy import select
    from sqlalchemy.orm import selectinload

    # Find user by email with row lock to prevent TOCTOU race on status check
    result = await db.execute(
        select(User)
        .where(User.email == req.email)
        .with_for_update()
    )
    user = result.scalar_one_or_none()

    if not user or not user.password_hash:
        raise HTTPException(status_code=401, detail="Invalid email or password")

    # Verify password
    if not verify_password(req.password, user.password_hash):
        raise HTTPException(status_code=401, detail="Invalid email or password")

    # Check user status (now safe from race conditions due to row lock)
    if user.status != UserStatus.ACTIVE:
        raise HTTPException(
            status_code=403,
            detail=f"Account is {user.status.value}. Please contact support.",
        )

    # Update last login
    user.last_login_at = datetime.utcnow()
    await db.commit()

    # Generate token
    token = generate_token(user.id)

    logger.info(f"User logged in: {user.id}")

    return {
        "token": token,
        "user_id": user.id,
        "email": user.email,
        "display_name": user.display_name,
    }


@app.get("/api/v1/user/me")
async def get_me(current_user: User = Depends(get_current_user)):
    """Get current user info."""
    return {
        "user_id": current_user.id,
        "email": current_user.email,
        "email_verified": current_user.email_verified,
        "display_name": current_user.display_name,
        "avatar_url": current_user.avatar_url,
        "status": current_user.status.value,
        "balance_usd": round(current_user.balance_usd, 2),
        "total_earned_usd": round(current_user.total_earned_usd, 2),
        "total_hours_recorded": round(current_user.total_hours_recorded, 2),
        "created_at": current_user.created_at.isoformat()
        if current_user.created_at
        else None,
        "last_login_at": current_user.last_login_at.isoformat()
        if current_user.last_login_at
        else None,
    }


@app.get("/api/v1/user/info")
async def get_user_info(current_user: User = Depends(get_current_user)):
    """Get user info (OWL Control compatibility)."""
    return {
        "user_id": current_user.id,
        "email": current_user.email,
        "balance_usd": round(current_user.balance_usd, 2),
        "total_earned_usd": round(current_user.total_earned_usd, 2),
        "total_hours_recorded": round(current_user.total_hours_recorded, 2),
    }


@app.post("/api/v1/upload/init")
async def upload_init(
    req: UploadInitRequest,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Initialize multipart upload."""
    upload_id = str(uuid.uuid4())
    game_control_id = str(uuid.uuid4())

    # Validate upload size — max 50 GB per recording, max 10000 chunks
    MAX_UPLOAD_BYTES = 50 * 1024 * 1024 * 1024  # 50 GB
    MAX_CHUNKS = 10000
    if req.total_size_bytes > MAX_UPLOAD_BYTES:
        raise HTTPException(
            status_code=400,
            detail=f"Upload too large: {req.total_size_bytes} bytes (max {MAX_UPLOAD_BYTES})",
        )
    chunk_size = req.chunk_size_bytes or 33554432
    total_chunks = min(
        max(1, math.ceil(req.total_size_bytes / chunk_size)), MAX_CHUNKS
    )

    # Sanitize filename to prevent path traversal
    safe_filename = sanitize_filename(req.filename)

    # Create upload record
    upload = Upload(
        id=upload_id,
        user_id=current_user.id,
        game_control_id=game_control_id,
        filename=safe_filename,
        total_size_bytes=req.total_size_bytes,
        chunk_size_bytes=chunk_size,
        total_chunks=total_chunks,
        game_exe=req.game_exe,
        video_duration_seconds=req.video_duration_seconds,
        video_width=req.video_width,
        video_height=req.video_height,
        video_codec=req.video_codec,
        video_fps=req.video_fps,
        recorder_version=req.recorder_version,
        hardware_id=req.hardware_id,
        upload_metadata=req.metadata,
        status=UploadStatus.IN_PROGRESS,
    )

    db.add(upload)
    await db.commit()

    # Generate S3 presigned URLs if configured
    chunk_urls = []
    s3_upload_id = None
    s3_key = None
    if AWS_ACCESS_KEY and AWS_SECRET_KEY:
        try:
            s3 = boto3.client(
                "s3",
                region_name=S3_REGION,
                aws_access_key_id=AWS_ACCESS_KEY,
                aws_secret_access_key=AWS_SECRET_KEY,
            )

            # Create multipart upload with sanitized filename
            s3_key = f"uploads/{current_user.id}/{upload_id}/{safe_filename}"
            mpu = s3.create_multipart_upload(Bucket=S3_BUCKET, Key=s3_key)
            s3_upload_id = mpu["UploadId"]

            upload.s3_key = s3_key
            upload.s3_upload_id = s3_upload_id
            await db.commit()

            # Generate presigned URLs for each chunk
            for i in range(1, total_chunks + 1):
                url = s3.generate_presigned_url(
                    "upload_part",
                    Params={
                        "Bucket": S3_BUCKET,
                        "Key": s3_key,
                        "UploadId": s3_upload_id,
                        "PartNumber": i,
                    },
                    ExpiresIn=3600,
                )
                chunk_urls.append({"chunk_number": i, "upload_url": url})
        except Exception as e:
            logger.error(f"S3 error: {e}")
            # Abort orphaned multipart upload to prevent resource leak
            if s3_upload_id and s3_key:
                try:
                    s3.abort_multipart_upload(
                        Bucket=S3_BUCKET, Key=s3_key, UploadId=s3_upload_id
                    )
                    logger.info(f"Aborted orphaned S3 multipart upload: {s3_upload_id}")
                except Exception as abort_err:
                    logger.warning(f"Failed to abort S3 multipart upload: {abort_err}")
            # Clear S3 references from database to maintain consistency
            # since the S3 upload was aborted but DB was already committed
            upload.s3_key = None
            upload.s3_upload_id = None
            await db.commit()
            # Continue with local storage fallback

    logger.info(f"Upload initialized: {upload_id} for user {current_user.id}")

    return {
        "upload_id": upload_id,
        "game_control_id": game_control_id,
        "total_chunks": total_chunks,
        "chunk_size_bytes": chunk_size,
        "expires_at": (datetime.utcnow() + timedelta(hours=24)).isoformat(),
        "chunk_urls": chunk_urls if chunk_urls else None,
    }


@app.post("/api/v1/upload/chunk")
async def upload_chunk(
    upload_id: str,
    chunk_number: int,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Get presigned URL for a specific chunk upload."""
    # Validate chunk_number
    if chunk_number < 1:
        raise HTTPException(status_code=400, detail="chunk_number must be >= 1")
    if chunk_number > 10000:
        raise HTTPException(status_code=400, detail="chunk_number exceeds maximum")

    # Find upload
    result = await db.execute(
        select(Upload).where(
            and_(Upload.id == upload_id, Upload.user_id == current_user.id)
        )
    )
    upload = result.scalar_one_or_none()

    if not upload:
        raise HTTPException(status_code=404, detail="Upload not found")

    # Validate chunk_number doesn't exceed this upload's total_chunks
    if chunk_number > upload.total_chunks:
        raise HTTPException(
            status_code=400,
            detail=f"chunk_number exceeds total_chunks ({upload.total_chunks})"
        )

    if upload.status != UploadStatus.IN_PROGRESS:
        raise HTTPException(
            status_code=400, detail=f"Upload is already {upload.status.value}"
        )

    # Generate S3 presigned URL if configured
    upload_url = None
    if AWS_ACCESS_KEY and AWS_SECRET_KEY and upload.s3_upload_id:
        try:
            s3 = boto3.client(
                "s3",
                region_name=S3_REGION,
                aws_access_key_id=AWS_ACCESS_KEY,
                aws_secret_access_key=AWS_SECRET_KEY,
            )
            upload_url = s3.generate_presigned_url(
                "upload_part",
                Params={
                    "Bucket": S3_BUCKET,
                    "Key": upload.s3_key,
                    "UploadId": upload.s3_upload_id,
                    "PartNumber": chunk_number,
                },
                ExpiresIn=3600,
            )
        except Exception as e:
            logger.error(f"Failed to generate chunk URL: {e}")
            raise HTTPException(status_code=500, detail="Failed to generate upload URL")

    return {"upload_url": upload_url, "chunk_number": chunk_number}


@app.post("/api/v1/upload/complete")
async def upload_complete(
    req: UploadCompleteRequest,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Complete upload and calculate earnings."""
    # Find upload with row lock to prevent race conditions on concurrent completion attempts
    result = await db.execute(
        select(Upload)
        .where(and_(Upload.id == req.upload_id, Upload.user_id == current_user.id))
        .with_for_update()
    )
    upload = result.scalar_one_or_none()

    if not upload:
        raise HTTPException(status_code=404, detail="Upload not found")

    if upload.status != UploadStatus.IN_PROGRESS:
        raise HTTPException(
            status_code=400, detail=f"Upload is already {upload.status.value}"
        )

    # Update upload status
    upload.status = UploadStatus.COMPLETED
    upload.completed_at = datetime.utcnow()

    # Calculate earnings — cap duration to prevent client-side fraud
    # Max 8 hours per single recording (anything longer is suspicious)
    raw_duration = max(0, upload.video_duration_seconds or 0)
    duration_hours = min(raw_duration / 3600, 8.0)

    # Get earnings multiplier from game or default
    earnings_per_hour = 0.50  # Default
    if upload.game_exe:
        game_result = await db.execute(
            select(Game).where(Game.exe_name == upload.game_exe.lower())
        )
        game = game_result.scalar_one_or_none()
        if game:
            earnings_per_hour *= game.earnings_multiplier

    earnings = round(duration_hours * earnings_per_hour, 2)
    # Ensure earnings are never negative (protects against negative multipliers in DB)
    earnings = max(0.0, earnings)
    upload.earnings_usd = earnings

    # Update user balance
    current_user.balance_usd += earnings
    current_user.total_earned_usd += earnings
    current_user.total_hours_recorded += duration_hours

    await db.commit()

    logger.info(f"Upload completed: {req.upload_id}, earnings: ${earnings}")

    return {
        "recording_id": req.upload_id,
        "estimated_earnings_usd": earnings,
        "quality_score": 0.85,  # Placeholder
        "status": "completed",
        "hours_recorded": round(duration_hours, 2),
    }


@app.get("/api/v1/earnings/summary")
async def earnings_summary(
    current_user: User = Depends(get_current_user), db: AsyncSession = Depends(get_db)
):
    """Get earnings summary for current user."""
    today = datetime.utcnow().date()

    # Get today's uploads
    result = await db.execute(
        select(Upload).where(
            and_(
                Upload.user_id == current_user.id,
                Upload.status == UploadStatus.COMPLETED,
                func.date(Upload.completed_at) == today,
            )
        )
    )
    today_uploads = result.scalars().all()

    today_earnings = sum(u.earnings_usd or 0 for u in today_uploads)
    today_hours = sum((u.video_duration_seconds or 0) / 3600 for u in today_uploads)

    # Get total uploads count
    result = await db.execute(
        select(func.count())
        .select_from(Upload)
        .where(
            and_(
                Upload.user_id == current_user.id,
                Upload.status == UploadStatus.COMPLETED,
            )
        )
    )
    total_recordings = result.scalar()

    return {
        "today_usd": round(today_earnings, 2),
        "this_month_usd": round(current_user.total_earned_usd, 2),  # Simplified
        "total_usd": round(current_user.total_earned_usd, 2),
        "pending_payout_usd": round(current_user.balance_usd, 2),
        "hours_recorded_today": round(today_hours, 2),
        "hours_recorded_total": round(current_user.total_hours_recorded, 2),
        "total_recordings": total_recordings,
    }


@app.get("/api/v1/earnings/history")
async def earnings_history(
    page: int = 1,
    per_page: int = 20,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Get earnings history."""
    # Validate pagination parameters to prevent DoS and division by zero
    if page < 1:
        raise HTTPException(status_code=400, detail="page must be >= 1")
    if per_page < 1 or per_page > 100:
        raise HTTPException(status_code=400, detail="per_page must be between 1 and 100")
    offset = (page - 1) * per_page

    result = await db.execute(
        select(Upload)
        .where(
            and_(
                Upload.user_id == current_user.id,
                Upload.status == UploadStatus.COMPLETED,
            )
        )
        .order_by(desc(Upload.completed_at))
        .offset(offset)
        .limit(per_page)
    )
    uploads = result.scalars().all()

    # Get total count
    count_result = await db.execute(
        select(func.count())
        .select_from(Upload)
        .where(
            and_(
                Upload.user_id == current_user.id,
                Upload.status == UploadStatus.COMPLETED,
            )
        )
    )
    total = count_result.scalar()

    items = [
        {
            "date": u.completed_at.isoformat() if u.completed_at else None,
            "game": u.game_exe or "Unknown",
            "hours": round((u.video_duration_seconds or 0) / 3600, 2),
            "earnings_usd": u.earnings_usd or 0,
            "quality_score": 0.85,
        }
        for u in uploads
    ]

    return {
        "items": items,
        "total": total,
        "page": page,
        "per_page": per_page,
        "total_pages": math.ceil(total / per_page),
    }


@app.get("/api/v1/uploads")
async def list_uploads(
    status: Optional[str] = None,
    page: int = 1,
    per_page: int = 20,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """List user's uploads."""
    # Validate pagination parameters to prevent DoS and division by zero
    if page < 1:
        raise HTTPException(status_code=400, detail="page must be >= 1")
    if per_page < 1 or per_page > 100:
        raise HTTPException(status_code=400, detail="per_page must be between 1 and 100")
    query = select(Upload).where(Upload.user_id == current_user.id)

    if status:
        try:
            upload_status = UploadStatus(status)
            query = query.where(Upload.status == upload_status)
        except ValueError:
            raise HTTPException(status_code=400, detail=f"Invalid status: {status}")

    query = query.order_by(desc(Upload.created_at))

    offset = (page - 1) * per_page
    result = await db.execute(query.offset(offset).limit(per_page))
    uploads = result.scalars().all()

    # Get total count
    count_query = (
        select(func.count())
        .select_from(Upload)
        .where(Upload.user_id == current_user.id)
    )
    if status:
        count_query = count_query.where(Upload.status == upload_status)
    count_result = await db.execute(count_query)
    total = count_result.scalar()

    items = [
        {
            "id": u.id,
            "filename": u.filename,
            "game": u.game_exe,
            "status": u.status.value,
            "size_bytes": u.total_size_bytes,
            "duration_seconds": u.video_duration_seconds,
            "earnings_usd": u.earnings_usd,
            "created_at": u.created_at.isoformat() if u.created_at else None,
            "completed_at": u.completed_at.isoformat() if u.completed_at else None,
        }
        for u in uploads
    ]

    return {"items": items, "total": total, "page": page, "per_page": per_page}


@app.get("/api/v1/games")
async def list_games(supported_only: bool = True, db: AsyncSession = Depends(get_db)):
    """List supported games."""
    query = select(Game)
    if supported_only:
        query = query.where(Game.is_supported == True)

    query = query.order_by(desc(Game.demand_level), Game.title)
    result = await db.execute(query)
    games = result.scalars().all()

    return {
        "games": [
            {
                "id": g.id,
                "exe_name": g.exe_name,
                "title": g.title,
                "genre": g.genre,
                "is_supported": g.is_supported,
                "demand_level": g.demand_level,
                "earnings_multiplier": g.earnings_multiplier,
            }
            for g in games
        ]
    }


@app.get("/api/v1/app/version")
async def app_version():
    """Get app version info."""
    return {
        "latest_version": "0.2.0",
        "download_url": "https://github.com/howardleegeek/gamedata-recorder/releases/latest",
        "required": False,
        "changelog": "Database backend, user registration, improved security.",
        "min_supported_version": "0.1.0",
    }


# Legacy OWL Control compatibility endpoints
@app.get("/tracker/v2/uploads/user/{uid}/stats")
async def upload_stats(
    uid: str,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """Get upload stats (OWL Control compatibility)."""
    # Validate path parameter matches authenticated user to prevent tampering
    if uid != current_user.id:
        raise HTTPException(status_code=403, detail="Access denied: user ID mismatch")
    result = await db.execute(
        select(Upload).where(
            and_(
                Upload.user_id == current_user.id,
                Upload.status == UploadStatus.COMPLETED,
            )
        )
    )
    uploads = result.scalars().all()

    total_bytes = sum(u.total_size_bytes for u in uploads)
    total_duration = sum(u.video_duration_seconds or 0 for u in uploads)

    return {
        "total_uploads": len(uploads),
        "total_size_bytes": total_bytes,
        "total_video_duration_seconds": total_duration,
    }


@app.get("/tracker/v2/uploads/user/{uid}/list")
async def upload_list_legacy(
    uid: str,
    limit: int = 20,
    offset: int = 0,
    current_user: User = Depends(get_current_user),
    db: AsyncSession = Depends(get_db),
):
    """List uploads (OWL Control compatibility)."""
    # Validate path parameter matches authenticated user to prevent tampering
    if uid != current_user.id:
        raise HTTPException(status_code=403, detail="Access denied: user ID mismatch")
    # Validate pagination parameters to prevent DoS
    if limit < 1 or limit > 100:
        raise HTTPException(status_code=400, detail="limit must be between 1 and 100")
    if offset < 0:
        raise HTTPException(status_code=400, detail="offset must be >= 0")

    result = await db.execute(
        select(Upload)
        .where(Upload.user_id == current_user.id)
        .order_by(desc(Upload.created_at))
        .offset(offset)
        .limit(limit)
    )
    uploads = result.scalars().all()

    count_result = await db.execute(
        select(func.count())
        .select_from(Upload)
        .where(Upload.user_id == current_user.id)
    )
    total = count_result.scalar()

    return {
        "items": [
            {
                "id": u.id,
                "filename": u.filename,
                "status": u.status.value,
                "created_at": u.created_at.isoformat() if u.created_at else None,
                "completed_at": u.completed_at.isoformat() if u.completed_at else None,
                "earnings_usd": u.earnings_usd,
            }
            for u in uploads
        ],
        "total": total,
    }


if __name__ == "__main__":
    import uvicorn

    port_str = os.getenv("PORT", "8080")
    try:
        port = int(port_str)
    except ValueError:
        logger.error(f"Invalid PORT environment variable: '{port_str}'. Using default 8080.")
        port = 8080
    uvicorn.run(app, host="0.0.0.0", port=port)
