# GameData Labs API Specification

## Base URL
```
Production: https://api.gamedatalabs.com
Staging:    https://api-staging.gamedatalabs.com
Local:      http://localhost:8080
```

Environment variable: `GAMEDATA_API_URL`

## Authentication

### OAuth 2.0 (User Login)
Users authenticate via browser-based OAuth. The recorder opens the user's browser to the login page, which redirects back with a token.

```
GET /auth/login?provider=google&redirect_uri=gamedata://auth/callback
→ Browser: Google OAuth consent screen
→ Redirect: gamedata://auth/callback?token=eyJ...
```

Supported providers: `google`, `discord`

### Bearer Token (API Calls)
All API calls use the token from OAuth:
```
Authorization: Bearer eyJ...
```

## Endpoints

### User

```
GET /api/v1/user/me
→ { user_id, email, display_name, balance_usd, total_earned_usd, total_hours_recorded }
```

### Recording Upload

```
POST /api/v1/upload/init
Body: {
  filename: "session_abc123.tar.gz",
  total_size_bytes: 1234567890,
  chunk_size_bytes: 33554432,  // 32 MB
  game_exe: "cs2.exe",
  game_title: "Counter-Strike 2",
  video_duration_seconds: 2700,
  video_width: 1920,
  video_height: 1080,
  video_codec: "hevc_nvenc",
  video_fps: 30.0,
  recorder_version: "0.2.0",
  hardware_id: "...",
  metadata: { ... }
}
→ { upload_id, total_chunks, presigned_urls: [...] }

PUT /api/v1/upload/{upload_id}/chunk/{chunk_number}
Body: <binary chunk data>
Headers: Content-SHA256: <sha256 hex>
→ { etag }

POST /api/v1/upload/{upload_id}/complete
Body: { etags: ["...", "..."] }
→ { recording_id, estimated_earnings_usd: 0.50, quality_score: 0.85 }

DELETE /api/v1/upload/{upload_id}/abort
→ { ok: true }
```

### Earnings

```
GET /api/v1/earnings/summary
→ {
  today_usd: 1.15,
  this_week_usd: 8.40,
  this_month_usd: 23.40,
  total_usd: 156.80,
  pending_payout_usd: 23.40,
  next_payout_date: "2026-04-15",
  hours_recorded_today: 2.3,
  hours_recorded_total: 312.5
}

GET /api/v1/earnings/history?page=1&per_page=20
→ { items: [{ date, game, hours, earnings_usd, quality_score }], total_pages }
```

### Payout

```
POST /api/v1/payout/request
Body: { method: "paypal", paypal_email: "user@example.com" }
→ { payout_id, amount_usd, estimated_arrival: "2026-04-17" }

GET /api/v1/payout/methods
→ { methods: ["paypal", "stripe", "crypto_usdc"] }
```

### App Updates

```
GET /api/v1/app/version
→ { latest_version: "0.2.1", download_url: "...", required: false, changelog: "..." }
```

### Health

```
GET /health
→ { status: "ok", version: "1.0.0" }
```

## Error Format

```json
{
  "error": {
    "code": "INVALID_TOKEN",
    "message": "Your session has expired. Please log in again.",
    "retry_after_seconds": null
  }
}
```

## Rate Limits

| Endpoint | Limit |
|----------|-------|
| Upload chunks | 100/min |
| Earnings queries | 30/min |
| All other | 60/min |

## Data Flow

```
Recorder → S3 (via presigned URL) → Lambda validator → SQS → Step Functions
  ↓
Quality scoring → Payment calculation → User balance update
  ↓
User sees earnings in app (polled every 60s)
```
