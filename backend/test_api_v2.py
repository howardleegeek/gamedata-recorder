"""Test suite for GameData Labs Backend v0.2.0 (Database version)

Run: python3 -m pytest test_api_v2.py -v

Requirements:
- PostgreSQL running locally
- Database 'gamedata' created
- User 'gamedata' with password 'gamedata'
"""

import pytest
import pytest_asyncio
from httpx import AsyncClient, ASGITransport
from sqlalchemy.ext.asyncio import create_async_engine, AsyncSession, async_sessionmaker
from sqlalchemy import select, delete

from main import app, get_db, Base
from models import User, Upload, Game, UserStatus, UploadStatus

# Test database
TEST_DATABASE_URL = "postgresql+asyncpg://gamedata:gamedata@localhost:5432/gamedata"

# Create test engine
test_engine = create_async_engine(TEST_DATABASE_URL)
TestSessionLocal = async_sessionmaker(
    test_engine, class_=AsyncSession, expire_on_commit=False
)


async def override_get_db():
    """Override database dependency for testing."""
    async with TestSessionLocal() as session:
        try:
            yield session
        finally:
            await session.close()


app.dependency_overrides[get_db] = override_get_db


@pytest_asyncio.fixture(scope="session", autouse=True)
async def cleanup_test_engine():
    """Cleanup fixture to dispose test engine after all tests."""
    yield
    await test_engine.dispose()


@pytest_asyncio.fixture(scope="function")
async def client():
    """Create test client."""
    # Create tables
    async with test_engine.begin() as conn:
        await conn.run_sync(Base.metadata.create_all)

    async with AsyncClient(
        transport=ASGITransport(app=app), base_url="http://test"
    ) as ac:
        yield ac

    # Cleanup: delete test data
    async with TestSessionLocal() as session:
        await session.execute(delete(Upload))
        await session.execute(delete(User))
        await session.execute(delete(Game))
        await session.commit()


@pytest_asyncio.fixture
async def test_user(client):
    """Create a test user and return token."""
    # Register user
    response = await client.post(
        "/api/v1/auth/register",
        json={
            "email": "test@example.com",
            "password": "TestPassword123",
            "display_name": "Test User",
        },
    )
    assert response.status_code == 200
    data = response.json()
    return data["token"], data["user_id"]


@pytest.mark.asyncio
async def test_health_check(client):
    """Test health endpoint."""
    response = await client.get("/health")
    assert response.status_code == 200
    data = response.json()
    assert data["status"] == "ok"
    assert "database" in data
    assert data["database"] == "connected"


@pytest.mark.asyncio
async def test_register_user(client):
    """Test user registration."""
    response = await client.post(
        "/api/v1/auth/register",
        json={
            "email": "newuser@example.com",
            "password": "SecurePass123",
            "display_name": "New User",
        },
    )
    assert response.status_code == 200
    data = response.json()
    assert "token" in data
    assert "user_id" in data
    assert data["email"] == "newuser@example.com"


@pytest.mark.asyncio
async def test_register_duplicate_email(client):
    """Test registration with duplicate email."""
    # First registration
    await client.post(
        "/api/v1/auth/register",
        json={"email": "duplicate@example.com", "password": "SecurePass123"},
    )

    # Second registration with same email
    response = await client.post(
        "/api/v1/auth/register",
        json={"email": "duplicate@example.com", "password": "SecurePass123"},
    )
    assert response.status_code == 400
    assert "already registered" in response.json()["detail"]


@pytest.mark.asyncio
async def test_login_success(client, test_user):
    """Test successful login."""
    response = await client.post(
        "/api/v1/auth/login",
        json={"email": "test@example.com", "password": "TestPassword123"},
    )
    assert response.status_code == 200
    data = response.json()
    assert "token" in data
    assert "user_id" in data


@pytest.mark.asyncio
async def test_login_wrong_password(client, test_user):
    """Test login with wrong password."""
    response = await client.post(
        "/api/v1/auth/login",
        json={"email": "test@example.com", "password": "WrongPassword"},
    )
    assert response.status_code == 401


@pytest.mark.asyncio
async def test_get_user_info(client, test_user):
    """Test getting user info."""
    token, user_id = test_user

    response = await client.get(
        "/api/v1/user/me", headers={"Authorization": f"Bearer {token}"}
    )
    assert response.status_code == 200
    data = response.json()
    assert data["user_id"] == user_id
    assert data["email"] == "test@example.com"
    assert "balance_usd" in data


@pytest.mark.asyncio
async def test_upload_init(client, test_user):
    """Test upload initialization."""
    token, user_id = test_user

    response = await client.post(
        "/api/v1/upload/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "test_recording.mp4",
            "total_size_bytes": 100000000,
            "game_exe": "cs2.exe",
            "video_duration_seconds": 1800,
            "video_codec": "h265",
        },
    )
    assert response.status_code == 200
    data = response.json()
    assert "upload_id" in data
    assert "total_chunks" in data
    assert data["total_chunks"] > 0


@pytest.mark.asyncio
async def test_upload_complete(client, test_user):
    """Test upload completion and earnings calculation."""
    token, user_id = test_user

    # Initialize upload
    init_response = await client.post(
        "/api/v1/upload/init",
        headers={"Authorization": f"Bearer {token}"},
        json={
            "filename": "test_recording.mp4",
            "total_size_bytes": 100000000,
            "game_exe": "cs2.exe",
            "video_duration_seconds": 3600,  # 1 hour
            "video_codec": "h265",
        },
    )
    upload_id = init_response.json()["upload_id"]

    # Complete upload
    response = await client.post(
        "/api/v1/upload/complete",
        headers={"Authorization": f"Bearer {token}"},
        json={"upload_id": upload_id, "etags": ["etag1", "etag2"]},
    )
    assert response.status_code == 200
    data = response.json()
    assert data["status"] == "completed"
    assert data["estimated_earnings_usd"] > 0
    assert data["hours_recorded"] == 1.0


@pytest.mark.asyncio
async def test_earnings_summary(client, test_user):
    """Test earnings summary."""
    token, user_id = test_user

    response = await client.get(
        "/api/v1/earnings/summary", headers={"Authorization": f"Bearer {token}"}
    )
    assert response.status_code == 200
    data = response.json()
    assert "today_usd" in data
    assert "total_usd" in data
    assert "pending_payout_usd" in data
    assert "total_recordings" in data


@pytest.mark.asyncio
async def test_list_uploads(client, test_user):
    """Test listing uploads."""
    token, user_id = test_user

    response = await client.get(
        "/api/v1/uploads", headers={"Authorization": f"Bearer {token}"}
    )
    assert response.status_code == 200
    data = response.json()
    assert "items" in data
    assert "total" in data


@pytest.mark.asyncio
async def test_unauthorized_access(client):
    """Test unauthorized access is blocked."""
    response = await client.get("/api/v1/user/me")
    assert response.status_code == 401


@pytest.mark.asyncio
async def test_invalid_token(client):
    """Test invalid token is rejected."""
    response = await client.get(
        "/api/v1/user/me", headers={"Authorization": "Bearer invalid_token"}
    )
    assert response.status_code == 401


@pytest.mark.asyncio
async def test_app_version(client):
    """Test app version endpoint."""
    response = await client.get("/api/v1/app/version")
    assert response.status_code == 200
    data = response.json()
    assert "latest_version" in data
    assert "download_url" in data


@pytest.mark.asyncio
async def test_owl_control_compat(client, test_user):
    """Test OWL Control compatibility endpoints."""
    token, user_id = test_user

    # Test /api/v1/user/info (OWL compat)
    response = await client.get("/api/v1/user/info", headers={"X-API-Key": token})
    assert response.status_code == 200
    assert "user_id" in response.json()

    # Test /tracker/v2/uploads/user/{uid}/stats (OWL compat)
    response = await client.get(
        f"/tracker/v2/uploads/user/{user_id}/stats",
        headers={"Authorization": f"Bearer {token}"},
    )
    assert response.status_code == 200
    data = response.json()
    assert "total_uploads" in data


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
