"""Contract tests for tracker multipart upload endpoints.

These tests verify that the backend responds with the exact fields
the recorder client (src/api/multipart_upload.rs) deserialises.

Run: python3 -m pytest test_tracker_upload.py -v
"""

import os

# Set DB env vars before importing main so module-level DATABASE_URL is valid
os.environ.setdefault("DB_USER", "gamedata")
os.environ.setdefault("DB_PASSWORD", "gamedata")
os.environ.setdefault("DB_HOST", "localhost")
os.environ.setdefault("DB_PORT", "5432")
os.environ.setdefault("DB_NAME", "gamedata")
os.environ.setdefault("API_SECRET", "test-api-secret-not-for-production")

import pytest
import pytest_asyncio
from httpx import AsyncClient, ASGITransport
from sqlalchemy.ext.asyncio import create_async_engine, AsyncSession, async_sessionmaker
from sqlalchemy import delete, create_engine
from sqlalchemy.pool import NullPool

from main import app, get_db, Base
from models import User, Upload, Game, UserStatus

TEST_DATABASE_URL = "postgresql+asyncpg://gamedata:gamedata@localhost:5432/gamedata"
SYNC_DATABASE_URL = TEST_DATABASE_URL.replace("postgresql+asyncpg://", "postgresql://")

test_engine = create_async_engine(TEST_DATABASE_URL, poolclass=NullPool)
TestSessionLocal = async_sessionmaker(
    test_engine, class_=AsyncSession, expire_on_commit=False
)


async def override_get_db():
    async with TestSessionLocal() as session:
        yield session


app.dependency_overrides[get_db] = override_get_db


@pytest_asyncio.fixture(scope="session", autouse=True)
async def cleanup_test_engine():
    yield
    await test_engine.dispose()


@pytest_asyncio.fixture(scope="function")
async def client():
    # Create tables (sync engine avoids asyncpg ENUM transaction issues in PG18)
    sync_engine = create_engine(SYNC_DATABASE_URL, poolclass=NullPool)
    Base.metadata.drop_all(sync_engine)
    Base.metadata.create_all(sync_engine)
    sync_engine.dispose()

    async with AsyncClient(
        transport=ASGITransport(app=app), base_url="http://test"
    ) as ac:
        yield ac

    sync_engine = create_engine(SYNC_DATABASE_URL, poolclass=NullPool)
    Base.metadata.drop_all(sync_engine)
    sync_engine.dispose()


@pytest_asyncio.fixture
async def test_user(client):
    """Register a test user, activate them, and return auth token + user_id."""
    response = await client.post(
        "/api/v1/auth/register",
        json={
            "email": "tracker-test@example.com",
            "password": "TrackerPass123",
            "display_name": "Tracker Test User",
        },
    )
    assert response.status_code == 200
    data = response.json()

    # Activate the user so get_current_user accepts the token
    sync_engine = create_engine(SYNC_DATABASE_URL, poolclass=NullPool)
    with sync_engine.connect() as conn:
        conn.execute(
            User.__table__.update()
            .where(User.id == data["user_id"])
            .values(status=UserStatus.ACTIVE)
        )
        conn.commit()
    sync_engine.dispose()

    return data["token"], data["user_id"]


# --- Happy path tests (recorder contract fidelity) ---


@pytest.mark.asyncio
async def test_tracker_upload_init_returns_expected_fields(client, test_user):
    """POST /tracker/upload/game_control/multipart/init

    Recorder reads these fields from InitMultipartUploadResponse:
      upload_id, game_control_id, total_chunks, chunk_size_bytes, expires_at
    """
    token, _ = test_user

    # Exact request shape the recorder sends
    response = await client.post(
        "/tracker/upload/game_control/multipart/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "session_2025-01-01_12-00-00.tar",
            "content_type": "application/x-tar",
            "total_size_bytes": 500_000_000,
            "chunk_size_bytes": 33_554_432,
            "video_duration_seconds": 1800.0,
            "video_width": 1920,
            "video_height": 1080,
            "video_codec": "hevc_nvenc",
            "video_fps": 30.0,
            "additional_metadata": {"game": "Counter-Strike 2", "map": "de_dust2"},
            "uploading_recorder_version": "0.2.0",
            "uploader_hwid": "HW-ABC123",
            "upload_timestamp": "2025-01-01T12:00:00+00:00",
        },
    )
    assert response.status_code == 200
    data = response.json()

    # Contract: every field the recorder reads
    assert "upload_id" in data
    assert isinstance(data["upload_id"], str)
    assert len(data["upload_id"]) > 0

    assert "game_control_id" in data
    assert isinstance(data["game_control_id"], str)
    assert len(data["game_control_id"]) > 0

    assert "total_chunks" in data
    assert isinstance(data["total_chunks"], int)
    assert data["total_chunks"] > 0

    assert "chunk_size_bytes" in data
    assert isinstance(data["chunk_size_bytes"], int)
    assert data["chunk_size_bytes"] > 0

    assert "expires_at" in data
    assert isinstance(data["expires_at"], int)
    assert data["expires_at"] > 0  # Unix timestamp in the future


@pytest.mark.asyncio
async def test_tracker_upload_chunk_returns_expected_fields(client, test_user):
    """POST /tracker/upload/game_control/multipart/chunk

    Recorder reads these fields from UploadMultipartChunkResponse:
      upload_url, chunk_number, expires_at
    """
    token, _ = test_user

    # Init first
    init_resp = await client.post(
        "/tracker/upload/game_control/multipart/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "chunk_test.tar",
            "total_size_bytes": 100_000_000,
            "uploader_hwid": "HW-TEST",
            "upload_timestamp": "2025-01-01T12:00:00+00:00",
        },
    )
    assert init_resp.status_code == 200
    upload_id = init_resp.json()["upload_id"]

    # Request chunk — recorder sends upload_id, chunk_number, chunk_hash
    response = await client.post(
        "/tracker/upload/game_control/multipart/chunk",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "upload_id": upload_id,
            "chunk_number": 1,
            "chunk_hash": "abc123def456",
        },
    )
    assert response.status_code == 200
    data = response.json()

    assert "upload_url" in data
    assert "chunk_number" in data
    assert data["chunk_number"] == 1
    assert "expires_at" in data
    assert isinstance(data["expires_at"], int)


@pytest.mark.asyncio
async def test_tracker_upload_complete_returns_expected_fields(client, test_user):
    """POST /tracker/upload/game_control/multipart/complete

    Recorder reads these fields from CompleteMultipartUploadResponse:
      success, game_control_id, object_key, message, verified (optional)
    """
    token, _ = test_user

    # Init
    init_resp = await client.post(
        "/tracker/upload/game_control/multipart/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "complete_test.tar",
            "total_size_bytes": 100_000_000,
            "video_duration_seconds": 3600.0,
            "uploader_hwid": "HW-TEST",
            "upload_timestamp": "2025-01-01T12:00:00+00:00",
        },
    )
    assert init_resp.status_code == 200
    init_data = init_resp.json()
    upload_id = init_data["upload_id"]
    game_control_id = init_data["game_control_id"]

    # Complete — recorder sends upload_id + chunk_etags
    response = await client.post(
        "/tracker/upload/game_control/multipart/complete",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "upload_id": upload_id,
            "chunk_etags": [
                {"chunk_number": 1, "etag": '"abc123"'},
                {"chunk_number": 2, "etag": '"def456"'},
            ],
        },
    )
    assert response.status_code == 200
    data = response.json()

    assert "success" in data
    assert data["success"] is True

    assert "game_control_id" in data
    assert data["game_control_id"] == game_control_id

    assert "object_key" in data
    assert isinstance(data["object_key"], str)
    assert len(data["object_key"]) > 0

    assert "message" in data
    assert isinstance(data["message"], str)

    # verified is optional but should be present (may be None)
    assert "verified" in data

    # Verify earnings were calculated
    async with TestSessionLocal() as session:
        upload = await session.get(Upload, upload_id)
        assert upload is not None
        assert upload.status.value == "completed"
        assert upload.earnings_usd > 0


@pytest.mark.asyncio
async def test_tracker_upload_abort_returns_expected_fields(client, test_user):
    """DELETE /tracker/upload/game_control/multipart/abort/{upload_id}

    Recorder reads these fields from AbortMultipartUploadResponse:
      success, message
    """
    token, _ = test_user

    # Init
    init_resp = await client.post(
        "/tracker/upload/game_control/multipart/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "abort_test.tar",
            "total_size_bytes": 100_000_000,
            "uploader_hwid": "HW-TEST",
            "upload_timestamp": "2025-01-01T12:00:00+00:00",
        },
    )
    assert init_resp.status_code == 200
    upload_id = init_resp.json()["upload_id"]

    # Abort — recorder sends DELETE with upload_id in path
    response = await client.delete(
        f"/tracker/upload/game_control/multipart/abort/{upload_id}",
        headers={"Authorization": f"Bearer {token}"},
    )
    assert response.status_code == 200
    data = response.json()

    assert "success" in data
    assert data["success"] is True

    assert "message" in data
    assert isinstance(data["message"], str)

    # Verify status changed to aborted
    async with TestSessionLocal() as session:
        upload = await session.get(Upload, upload_id)
        assert upload is not None
        assert upload.status.value == "aborted"


# --- Full lifecycle test (realistic recorder flow) ---


@pytest.mark.asyncio
async def test_tracker_upload_full_lifecycle(client, test_user):
    """Simulate the complete recorder upload flow: init → chunk → complete."""
    token, _ = test_user

    # Step 1: Init
    init_resp = await client.post(
        "/tracker/upload/game_control/multipart/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "lifecycle_test.tar",
            "content_type": "application/x-tar",
            "total_size_bytes": 33_554_432,
            "chunk_size_bytes": 33_554_432,
            "video_duration_seconds": 900.0,
            "video_codec": "hevc_nvenc",
            "video_fps": 60.0,
            "additional_metadata": {"game": "Valorant"},
            "uploading_recorder_version": "0.3.0",
            "uploader_hwid": "HW-LIFECYCLE",
            "upload_timestamp": "2025-06-01T00:00:00+00:00",
        },
    )
    assert init_resp.status_code == 200
    init_data = init_resp.json()
    assert init_data["total_chunks"] == 1

    # Step 2: Get chunk URL
    chunk_resp = await client.post(
        "/tracker/upload/game_control/multipart/chunk",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "upload_id": init_data["upload_id"],
            "chunk_number": 1,
            "chunk_hash": "test_hash_123",
        },
    )
    assert chunk_resp.status_code == 200
    chunk_data = chunk_resp.json()
    assert chunk_data["chunk_number"] == 1
    # upload_url can be None when no S3 configured (local mode)
    assert "upload_url" in chunk_data
    assert "expires_at" in chunk_data

    # Step 3: Complete
    complete_resp = await client.post(
        "/tracker/upload/game_control/multipart/complete",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "upload_id": init_data["upload_id"],
            "chunk_etags": [{"chunk_number": 1, "etag": '"abc123"'}],
        },
    )
    assert complete_resp.status_code == 200
    complete_data = complete_resp.json()
    assert complete_data["success"] is True
    assert complete_data["game_control_id"] == init_data["game_control_id"]
    assert complete_data["verified"] is None


# --- Error cases ---


@pytest.mark.asyncio
async def test_tracker_upload_init_no_auth(client):
    """Tracker init without auth must return 401."""
    response = await client.post(
        "/tracker/upload/game_control/multipart/init",
        json={
            "filename": "noauth.tar",
            "total_size_bytes": 1000,
            "uploader_hwid": "HW-TEST",
            "upload_timestamp": "2025-01-01T12:00:00+00:00",
        },
    )
    assert response.status_code == 401


@pytest.mark.asyncio
async def test_tracker_upload_chunk_no_auth(client):
    """Tracker chunk without auth must return 401."""
    response = await client.post(
        "/tracker/upload/game_control/multipart/chunk",
        json={"upload_id": "fake", "chunk_number": 1},
    )
    assert response.status_code == 401


@pytest.mark.asyncio
async def test_tracker_upload_complete_no_auth(client):
    """Tracker complete without auth must return 401."""
    response = await client.post(
        "/tracker/upload/game_control/multipart/complete",
        json={"upload_id": "fake", "chunk_etags": []},
    )
    assert response.status_code == 401


@pytest.mark.asyncio
async def test_tracker_upload_abort_no_auth(client):
    """Tracker abort without auth must return 401."""
    response = await client.delete(
        "/tracker/upload/game_control/multipart/abort/fake-upload",
    )
    assert response.status_code == 401


@pytest.mark.asyncio
async def test_tracker_upload_init_exceeds_max(client, test_user):
    """Tracker init with file exceeding MAX_UPLOAD_BYTES must be rejected."""
    token, _ = test_user

    response = await client.post(
        "/tracker/upload/game_control/multipart/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "huge_file.tar",
            "total_size_bytes": 60 * 1024 * 1024 * 1024,  # 60 GB > 50 GB max
            "uploader_hwid": "HW-TEST",
            "upload_timestamp": "2025-01-01T12:00:00+00:00",
        },
    )
    assert response.status_code == 400
    data = response.json()
    assert "too large" in data["detail"].lower()


@pytest.mark.asyncio
async def test_tracker_upload_chunk_nonexistent_upload(client, test_user):
    """Tracker chunk with non-existent upload_id returns 404."""
    token, _ = test_user

    response = await client.post(
        "/tracker/upload/game_control/multipart/chunk",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "upload_id": "00000000-0000-0000-0000-000000000000",
            "chunk_number": 1,
            "chunk_hash": "test",
        },
    )
    assert response.status_code == 404


@pytest.mark.asyncio
async def test_tracker_upload_complete_nonexistent_upload(client, test_user):
    """Tracker complete with non-existent upload_id returns 404."""
    token, _ = test_user

    response = await client.post(
        "/tracker/upload/game_control/multipart/complete",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "upload_id": "00000000-0000-0000-0000-000000000000",
            "chunk_etags": [],
        },
    )
    assert response.status_code == 404


@pytest.mark.asyncio
async def test_tracker_upload_abort_nonexistent_upload(client, test_user):
    """Tracker abort with non-existent upload_id returns 404."""
    token, _ = test_user

    response = await client.delete(
        "/tracker/upload/game_control/multipart/abort/00000000-0000-0000-0000-000000000000",
        headers={"Authorization": f"Bearer {token}"},
    )
    assert response.status_code == 404


@pytest.mark.asyncio
async def test_tracker_upload_complete_already_aborted(client, test_user):
    """Completing an already-aborted upload returns 400."""
    token, _ = test_user

    init_resp = await client.post(
        "/tracker/upload/game_control/multipart/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "already_aborted.tar",
            "total_size_bytes": 100_000_000,
            "uploader_hwid": "HW-TEST",
            "upload_timestamp": "2025-01-01T12:00:00+00:00",
        },
    )
    assert init_resp.status_code == 200
    upload_id = init_resp.json()["upload_id"]

    # Abort first
    await client.delete(
        f"/tracker/upload/game_control/multipart/abort/{upload_id}",
        headers={"Authorization": f"Bearer {token}"},
    )

    # Try to complete
    response = await client.post(
        "/tracker/upload/game_control/multipart/complete",
        headers={"Authorization": f"Bearer {token}"},
        json={"upload_id": upload_id, "chunk_etags": []},
    )
    assert response.status_code == 400


@pytest.mark.asyncio
async def test_tracker_upload_chunk_exceeds_total(client, test_user):
    """Chunk number exceeding total_chunks must be rejected."""
    token, _ = test_user

    init_resp = await client.post(
        "/tracker/upload/game_control/multipart/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "small.tar",
            "total_size_bytes": 33_554_432,  # 1 chunk
            "uploader_hwid": "HW-TEST",
            "upload_timestamp": "2025-01-01T12:00:00+00:00",
        },
    )
    assert init_resp.status_code == 200
    upload_id = init_resp.json()["upload_id"]

    response = await client.post(
        "/tracker/upload/game_control/multipart/chunk",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "upload_id": upload_id,
            "chunk_number": 999,
            "chunk_hash": "test",
        },
    )
    assert response.status_code == 400


# --- Backward compatibility: existing /api/v1/upload/* still works ---


@pytest.mark.asyncio
async def test_existing_upload_init_still_works(client, test_user):
    """Existing /api/v1/upload/init must not regress."""
    token, _ = test_user

    response = await client.post(
        "/api/v1/upload/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "existing_test.mp4",
            "total_size_bytes": 100_000_000,
            "game_exe": "cs2.exe",
            "video_duration_seconds": 1800,
        },
    )
    assert response.status_code == 200
    data = response.json()
    assert "upload_id" in data
    assert "chunk_urls" in data


@pytest.mark.asyncio
async def test_existing_upload_complete_still_works(client, test_user):
    """Existing /api/v1/upload/complete must not regress."""
    token, _ = test_user

    init_resp = await client.post(
        "/api/v1/upload/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "existing_complete_test.mp4",
            "total_size_bytes": 100_000_000,
            "video_duration_seconds": 1800,
        },
    )
    assert init_resp.status_code == 200
    upload_id = init_resp.json()["upload_id"]

    response = await client.post(
        "/api/v1/upload/complete",
        headers={"Authorization": f"Bearer {token}"},
        json={"upload_id": upload_id, "etags": ["etag1"]},
    )
    assert response.status_code == 200
    data = response.json()
    assert data["status"] == "completed"
    assert "recording_id" in data
    assert "estimated_earnings_usd" in data
