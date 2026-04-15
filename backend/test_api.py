"""Run: python3 test_api.py"""
import atexit
from main import app
from fastapi.testclient import TestClient


def run_tests():
    """Run all API tests."""
    with TestClient(app) as client:
        _run_tests_with_client(client)


def _run_tests_with_client(client):
    """Run all API tests with the provided client."""
    # 1. Health
    r = client.get("/health")
    assert r.status_code == 200
    print(f"[PASS] Health: {r.json()}")

    # 2. Login
    r = client.post("/api/v1/auth/login", json={"email": "test@gamedatalabs.com", "password": "testpassword123"})
    assert r.status_code == 200
    token = r.json()["token"]
    user_id = r.json()["user_id"]
    print(f"[PASS] Login: user_id={user_id}")

    headers = {"Authorization": f"Bearer {token}"}

    # 3. User info
    r = client.get("/api/v1/user/me", headers=headers)
    assert r.status_code == 200
    assert r.json()["balance_usd"] == 0.0
    print(f"[PASS] User info: balance=${r.json()['balance_usd']}")

    # 4. Upload init
    r = client.post("/api/v1/upload/init", headers=headers, json={
        "filename": "session_test.tar.gz",
        "total_size_bytes": 500_000_000,
        "game_exe": "cs2.exe",
        "video_duration_seconds": 1800,
        "video_codec": "hevc_nvenc",
        "video_fps": 30.0,
    })
    assert r.status_code == 200
    upload_id = r.json()["upload_id"]
    print(f"[PASS] Upload init: {upload_id}, chunks={r.json()['total_chunks']}")

    # 5. Upload chunk URL
    r = client.post("/api/v1/upload/chunk", headers=headers, params={"upload_id": upload_id, "chunk_number": 1})
    assert r.status_code == 200
    print(f"[PASS] Chunk URL: {r.json()['upload_url'][:50]}...")

    # 6. Upload complete
    r = client.post("/api/v1/upload/complete", headers=headers, json={"upload_id": upload_id, "etags": ["abc"]})
    assert r.status_code == 200
    assert r.json()["estimated_earnings_usd"] == 0.25  # 0.5 hours * $0.50/hr
    print(f"[PASS] Upload complete: earned ${r.json()['estimated_earnings_usd']}")

    # 7. Earnings
    r = client.get("/api/v1/earnings/summary", headers=headers)
    assert r.status_code == 200
    assert r.json()["total_usd"] == 0.25
    print(f"[PASS] Earnings: total=${r.json()['total_usd']}, hours={r.json()['hours_recorded_total']}")

    # 8. OWL Control compat
    r = client.get("/api/v1/user/info", headers={"X-API-Key": token})
    assert r.status_code == 200
    print(f"[PASS] OWL compat: user_id={r.json()['user_id']}")

    # 9. Upload stats
    r = client.get(f"/tracker/v2/uploads/user/{user_id}/stats", headers=headers)
    assert r.status_code == 200
    print(f"[PASS] Stats: {r.json()['total_uploads']} uploads, {r.json()['total_size_bytes']} bytes")

    # 10. App version
    r = client.get("/api/v1/app/version")
    assert r.status_code == 200
    print(f"[PASS] Version: {r.json()['latest_version']}")

    print("\n=== ALL 10 TESTS PASSED ===")


if __name__ == "__main__":
    run_tests()
