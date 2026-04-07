"""
GameData Labs Backend MVP
Minimal API server for the GameData Recorder client.
Handles: auth, upload init/complete, earnings query.
"""

import os
import uuid
import time
import hmac
import hashlib
import json
from datetime import datetime, timedelta
from pathlib import Path

import boto3
from fastapi import FastAPI, HTTPException, Header, Depends
from fastapi.middleware.cors import CORSMiddleware
from pydantic import BaseModel
from typing import Optional

app = FastAPI(title="GameData Labs API", version="0.1.0")

# CORS configuration - restrict to specific origins in production
ALLOWED_ORIGINS = os.getenv(
    "ALLOWED_ORIGINS", "http://localhost:3000,http://localhost:8080"
).split(",")

app.add_middleware(
    CORSMiddleware,
    allow_origins=ALLOWED_ORIGINS,
    allow_methods=["GET", "POST", "PUT", "DELETE"],
    allow_headers=["Authorization", "Content-Type", "X-API-Key"],
    allow_credentials=True,
)

# --- Config ---

S3_BUCKET = os.getenv("S3_BUCKET", "gamedata-recordings")
S3_REGION = os.getenv("S3_REGION", "us-east-1")
AWS_ACCESS_KEY = os.getenv("AWS_ACCESS_KEY_ID")
AWS_SECRET_KEY = os.getenv("AWS_SECRET_ACCESS_KEY")
DATA_DIR = Path(os.getenv("DATA_DIR", "./data"))
DATA_DIR.mkdir(parents=True, exist_ok=True)

# --- Simple file-based storage (replace with PostgreSQL for production) ---


def get_db_path(collection: str) -> Path:
    p = DATA_DIR / collection
    p.mkdir(parents=True, exist_ok=True)
    return p


def save_record(collection: str, record_id: str, data: dict):
    path = get_db_path(collection) / f"{record_id}.json"
    path.write_text(json.dumps(data, default=str, indent=2))


def load_record(collection: str, record_id: str) -> Optional[dict]:
    path = get_db_path(collection) / f"{record_id}.json"
    if path.exists():
        return json.loads(path.read_text())
    return None


def list_records(collection: str) -> list[dict]:
    path = get_db_path(collection)
    records = []
    for f in sorted(path.glob("*.json"), key=lambda x: x.stat().st_mtime, reverse=True):
        records.append(json.loads(f.read_text()))
    return records


# --- Auth (MVP: simple token-based, replace with OAuth for production) ---

# SECURITY: API_SECRET must be set in environment variable for production
# Generate a secure secret with: openssl rand -hex 32
API_SECRET = os.getenv("API_SECRET")
if not API_SECRET:
    import secrets
    import warnings

    warnings.warn(
        "API_SECRET not set! Using a temporary random secret. "
        "Set API_SECRET environment variable for production!",
        RuntimeWarning,
    )
    API_SECRET = secrets.token_hex(32)


def generate_token(user_id: str) -> str:
    payload = f"{user_id}:{int(time.time())}"
    sig = hmac.new(API_SECRET.encode(), payload.encode(), hashlib.sha256).hexdigest()[
        :16
    ]
    return f"{payload}:{sig}"


def verify_token(token: str) -> Optional[str]:
    """Returns user_id if token is valid, None otherwise."""
    parts = token.split(":")
    if len(parts) != 3:
        return None
    user_id, timestamp, sig = parts
    expected_payload = f"{user_id}:{timestamp}"
    expected_sig = hmac.new(
        API_SECRET.encode(), expected_payload.encode(), hashlib.sha256
    ).hexdigest()[:16]
    if not hmac.compare_digest(sig, expected_sig):
        return None
    # Token valid for 30 days
    if int(time.time()) - int(timestamp) > 30 * 86400:
        return None
    return user_id


async def get_current_user(
    authorization: str = Header(None), x_api_key: str = Header(None)
):
    """Extract user from Bearer token or X-API-Key header."""
    token = None
    if authorization and authorization.startswith("Bearer "):
        token = authorization[7:]
    elif x_api_key:
        token = x_api_key

    if not token:
        raise HTTPException(status_code=401, detail="Missing authentication")

    user_id = verify_token(token)
    if not user_id:
        # MVP fallback: treat any sk_ prefixed key as valid (OWL Control compat)
        if token.startswith("sk_"):
            user_id = f"legacy_{token[3:11]}"
        else:
            raise HTTPException(status_code=401, detail="Invalid token")

    return user_id


# --- Models ---


class LoginRequest(BaseModel):
    email: str
    provider: str = "email"  # email, google, discord


class UploadInitRequest(BaseModel):
    filename: str
    total_size_bytes: int
    chunk_size_bytes: Optional[int] = 33554432  # 32 MB
    game_exe: Optional[str] = None
    video_duration_seconds: Optional[float] = None
    video_width: Optional[int] = None
    video_height: Optional[int] = None
    video_codec: Optional[str] = None
    video_fps: Optional[float] = None
    recorder_version: Optional[str] = None
    hardware_id: Optional[str] = None
    metadata: Optional[dict] = None
    # OWL Control compat fields
    tags: Optional[list[str]] = None
    video_filename: Optional[str] = None
    control_filename: Optional[str] = None
    additional_metadata: Optional[dict] = None
    uploading_recorder_version: Optional[str] = None
    uploading_owl_control_version: Optional[str] = None
    uploader_hwid: Optional[str] = None
    upload_timestamp: Optional[str] = None
    content_type: Optional[str] = None


class UploadCompleteRequest(BaseModel):
    upload_id: str
    etags: list[str] = []
    chunk_etags: Optional[list[dict]] = None  # OWL compat: [{chunk_number, etag}]


# --- Endpoints ---


@app.get("/health")
async def health():
    return {"status": "ok", "version": "0.1.0"}


# OWL Control compat: /api/v1/user/info
@app.get("/api/v1/user/info")
async def user_info(user_id: str = Depends(get_current_user)):
    user = load_record("users", user_id)
    if not user:
        user = {
            "user_id": user_id,
            "email": f"{user_id}@gamedatalabs.com",
            "created_at": datetime.utcnow().isoformat(),
            "balance_usd": 0.0,
            "total_earned_usd": 0.0,
            "total_hours_recorded": 0.0,
        }
        save_record("users", user_id, user)
    return user


@app.get("/api/v1/user/me")
async def user_me(user_id: str = Depends(get_current_user)):
    return await user_info(user_id)


@app.post("/api/v1/auth/login")
async def login(req: LoginRequest):
    """MVP login: creates user and returns token. Replace with OAuth for production."""
    user_id = f"user_{hashlib.md5(req.email.encode()).hexdigest()[:12]}"
    user = load_record("users", user_id)
    if not user:
        user = {
            "user_id": user_id,
            "email": req.email,
            "provider": req.provider,
            "created_at": datetime.utcnow().isoformat(),
            "balance_usd": 0.0,
            "total_earned_usd": 0.0,
            "total_hours_recorded": 0.0,
        }
        save_record("users", user_id, user)
    token = generate_token(user_id)
    return {"token": token, "user_id": user_id}


# --- Upload (OWL Control compatible multipart) ---


@app.post("/tracker/upload/game_control/multipart/init")
@app.post("/api/v1/upload/init")
async def upload_init(req: UploadInitRequest, user_id: str = Depends(get_current_user)):
    upload_id = str(uuid.uuid4())
    game_control_id = str(uuid.uuid4())

    chunk_size = req.chunk_size_bytes or 33554432
    total_chunks = max(1, (req.total_size_bytes + chunk_size - 1) // chunk_size)

    upload = {
        "upload_id": upload_id,
        "game_control_id": game_control_id,
        "user_id": user_id,
        "filename": req.filename,
        "total_size_bytes": req.total_size_bytes,
        "chunk_size_bytes": chunk_size,
        "total_chunks": total_chunks,
        "status": "in_progress",
        "created_at": datetime.utcnow().isoformat(),
        "game_exe": req.game_exe,
        "video_duration_seconds": req.video_duration_seconds,
        "video_codec": req.video_codec,
        "video_fps": req.video_fps,
        "recorder_version": req.uploading_recorder_version or req.recorder_version,
        "metadata": req.metadata or req.additional_metadata,
    }
    save_record("uploads", upload_id, upload)

    # Generate presigned URLs for S3 (or local storage for MVP)
    s3_key = f"uploads/{user_id}/{upload_id}/{req.filename}"

    if AWS_ACCESS_KEY:
        # Real S3 multipart upload
        s3 = boto3.client(
            "s3",
            region_name=S3_REGION,
            aws_access_key_id=AWS_ACCESS_KEY,
            aws_secret_access_key=AWS_SECRET_KEY,
        )
        mpu = s3.create_multipart_upload(Bucket=S3_BUCKET, Key=s3_key)
        upload["s3_upload_id"] = mpu["UploadId"]
        upload["s3_key"] = s3_key
        save_record("uploads", upload_id, upload)

    return {
        "upload_id": upload_id,
        "game_control_id": game_control_id,
        "total_chunks": total_chunks,
        "chunk_size_bytes": chunk_size,
        "expires_at": (datetime.utcnow() + timedelta(hours=24)).isoformat(),
    }


@app.post("/tracker/upload/game_control/multipart/chunk")
@app.post("/api/v1/upload/chunk")
async def upload_chunk(
    upload_id: str = "",
    chunk_number: int = 1,
    user_id: str = Depends(get_current_user),
):
    """Return a presigned URL for uploading a chunk."""
    upload = load_record("uploads", upload_id)
    if not upload:
        raise HTTPException(status_code=404, detail="Upload not found")

    if AWS_ACCESS_KEY and "s3_upload_id" in upload:
        s3 = boto3.client(
            "s3",
            region_name=S3_REGION,
            aws_access_key_id=AWS_ACCESS_KEY,
            aws_secret_access_key=AWS_SECRET_KEY,
        )
        url = s3.generate_presigned_url(
            "upload_part",
            Params={
                "Bucket": S3_BUCKET,
                "Key": upload["s3_key"],
                "UploadId": upload["s3_upload_id"],
                "PartNumber": chunk_number,
            },
            ExpiresIn=3600,
        )
    else:
        # Local storage fallback
        url = (
            f"http://localhost:8080/api/v1/upload/{upload_id}/chunk/{chunk_number}/data"
        )

    return {
        "upload_url": url,
        "chunk_number": chunk_number,
        "expires_at": (datetime.utcnow() + timedelta(hours=1)).isoformat(),
    }


@app.post("/tracker/upload/game_control/multipart/complete")
@app.post("/api/v1/upload/complete")
async def upload_complete(
    req: UploadCompleteRequest, user_id: str = Depends(get_current_user)
):
    upload = load_record("uploads", req.upload_id)
    if not upload:
        raise HTTPException(status_code=404, detail="Upload not found")

    upload["status"] = "completed"
    upload["completed_at"] = datetime.utcnow().isoformat()

    # Calculate earnings (MVP: flat rate per hour)
    duration_hours = (upload.get("video_duration_seconds") or 0) / 3600
    earnings = round(duration_hours * 0.50, 2)  # $0.50/hour Tier 1
    upload["earnings_usd"] = earnings

    save_record("uploads", req.upload_id, upload)

    # Update user balance
    user = load_record("users", user_id) or {}
    user["balance_usd"] = round(user.get("balance_usd", 0) + earnings, 2)
    user["total_earned_usd"] = round(user.get("total_earned_usd", 0) + earnings, 2)
    user["total_hours_recorded"] = round(
        user.get("total_hours_recorded", 0) + duration_hours, 2
    )
    save_record("users", user_id, user)

    return {
        "recording_id": req.upload_id,
        "estimated_earnings_usd": earnings,
        "quality_score": 0.85,  # placeholder
        "status": "completed",
    }


@app.delete("/tracker/upload/game_control/multipart/abort/{upload_id}")
@app.delete("/api/v1/upload/{upload_id}/abort")
async def upload_abort(upload_id: str, user_id: str = Depends(get_current_user)):
    upload = load_record("uploads", upload_id)
    if upload:
        upload["status"] = "aborted"
        save_record("uploads", upload_id, upload)
    return {"ok": True}


# --- Earnings ---


@app.get("/api/v1/earnings/summary")
async def earnings_summary(user_id: str = Depends(get_current_user)):
    user = load_record("users", user_id) or {}
    uploads = [
        u
        for u in list_records("uploads")
        if u.get("user_id") == user_id and u.get("status") == "completed"
    ]

    today = datetime.utcnow().date()
    today_earnings = sum(
        u.get("earnings_usd", 0)
        for u in uploads
        if u.get("completed_at", "")[:10] == str(today)
    )
    today_hours = sum(
        (u.get("video_duration_seconds") or 0) / 3600
        for u in uploads
        if u.get("completed_at", "")[:10] == str(today)
    )

    return {
        "today_usd": round(today_earnings, 2),
        "this_month_usd": round(user.get("total_earned_usd", 0), 2),
        "total_usd": round(user.get("total_earned_usd", 0), 2),
        "pending_payout_usd": round(user.get("balance_usd", 0), 2),
        "hours_recorded_today": round(today_hours, 2),
        "hours_recorded_total": round(user.get("total_hours_recorded", 0), 2),
        "total_recordings": len(uploads),
    }


@app.get("/api/v1/earnings/history")
async def earnings_history(
    page: int = 1,
    per_page: int = 20,
    user_id: str = Depends(get_current_user),
):
    uploads = [
        u
        for u in list_records("uploads")
        if u.get("user_id") == user_id and u.get("status") == "completed"
    ]
    start = (page - 1) * per_page
    end = start + per_page
    items = [
        {
            "date": u.get("completed_at", "")[:10],
            "game": u.get("game_exe", "Unknown"),
            "hours": round((u.get("video_duration_seconds") or 0) / 3600, 2),
            "earnings_usd": u.get("earnings_usd", 0),
            "quality_score": 0.85,
        }
        for u in uploads[start:end]
    ]
    return {
        "items": items,
        "total_pages": max(1, (len(uploads) + per_page - 1) // per_page),
    }


# --- Upload stats (OWL Control compat) ---


@app.get("/tracker/v2/uploads/user/{uid}/stats")
async def upload_stats(uid: str, user_id: str = Depends(get_current_user)):
    uploads = [
        u
        for u in list_records("uploads")
        if u.get("user_id") == user_id and u.get("status") == "completed"
    ]
    total_bytes = sum(u.get("total_size_bytes", 0) for u in uploads)
    total_duration = sum(u.get("video_duration_seconds", 0) for u in uploads)
    return {
        "total_uploads": len(uploads),
        "total_size_bytes": total_bytes,
        "total_video_duration_seconds": total_duration,
    }


@app.get("/tracker/v2/uploads/user/{uid}/list")
async def upload_list(
    uid: str,
    limit: int = 20,
    offset: int = 0,
    user_id: str = Depends(get_current_user),
):
    uploads = [u for u in list_records("uploads") if u.get("user_id") == user_id]
    return {"items": uploads[offset : offset + limit], "total": len(uploads)}


# --- App version check ---


@app.get("/api/v1/app/version")
async def app_version():
    return {
        "latest_version": "0.2.0",
        "download_url": "https://github.com/howardleegeek/gamedata-recorder/releases/latest",
        "required": False,
        "changelog": "Initial release with H.265 encoding and auto-record mode.",
    }


if __name__ == "__main__":
    import uvicorn

    uvicorn.run(app, host="0.0.0.0", port=8080)
