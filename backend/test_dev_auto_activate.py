import asyncio
import pytest
from httpx import AsyncClient, ASGITransport
from sqlalchemy import select, text

from main import app, SessionLocal
from models import User, AuditLog, UserStatus


@pytest.fixture(scope="module")
def event_loop():
    loop = asyncio.new_event_loop()
    asyncio.set_event_loop(loop)
    yield loop
    loop.close()


def _clean_db():
    async def _clean():
        async with SessionLocal() as session:
            await session.execute(text("DELETE FROM audit_logs"))
            await session.execute(text("DELETE FROM users"))
            await session.commit()

    return _clean


@pytest.mark.asyncio(loop_scope="module")
async def test_dev_mode_auto_activates_user(event_loop):
    await _clean_db()()
    import main as main_module

    original_env = main_module.ENVIRONMENT
    main_module.ENVIRONMENT = "development"

    async with AsyncClient(
        transport=ASGITransport(app=app), base_url="http://test"
    ) as client:
        try:
            resp = await client.post(
                "/api/v1/auth/register",
                json={
                    "email": "devuser@test.com",
                    "password": "DevPass123",
                    "display_name": "Dev User",
                },
            )
            assert resp.status_code == 200
            data = resp.json()
            assert data["message"] == "Registration successful."
            assert "Please verify your email." not in data["message"]

            async with SessionLocal() as session:
                result = await session.execute(
                    select(User).where(User.email == "devuser@test.com")
                )
                user = result.scalar_one_or_none()
                assert user is not None
                assert user.status == UserStatus.ACTIVE

                audit_result = await session.execute(
                    select(AuditLog).where(AuditLog.action == "user_registered")
                )
                audit = audit_result.scalar_one_or_none()
                assert audit is not None
                assert audit.details == {
                    "status": "active",
                    "reason": "dev-auto-activated",
                }

            resp = await client.post(
                "/api/v1/auth/login",
                json={"email": "devuser@test.com", "password": "DevPass123"},
            )
            assert resp.status_code == 200
            assert "token" in resp.json()
        finally:
            main_module.ENVIRONMENT = original_env


@pytest.mark.asyncio(loop_scope="module")
async def test_production_mode_stays_pending(event_loop):
    await _clean_db()()
    import main as main_module

    original_env = main_module.ENVIRONMENT
    main_module.ENVIRONMENT = "production"

    async with AsyncClient(
        transport=ASGITransport(app=app), base_url="http://test"
    ) as client:
        try:
            resp = await client.post(
                "/api/v1/auth/register",
                json={
                    "email": "produser@test.com",
                    "password": "ProdPass123",
                    "display_name": "Prod User",
                },
            )
            assert resp.status_code == 200
            data = resp.json()
            assert (
                data["message"] == "Registration successful. Please verify your email."
            )

            async with SessionLocal() as session:
                result = await session.execute(
                    select(User).where(User.email == "produser@test.com")
                )
                user = result.scalar_one_or_none()
                assert user is not None
                assert user.status == UserStatus.PENDING_VERIFICATION

            resp = await client.post(
                "/api/v1/auth/login",
                json={"email": "produser@test.com", "password": "ProdPass123"},
            )
            assert resp.status_code == 403
        finally:
            main_module.ENVIRONMENT = original_env
