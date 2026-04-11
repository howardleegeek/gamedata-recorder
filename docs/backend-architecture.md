# GameData Labs -- Backend Architecture

> Complete system design for ingesting gameplay recordings, processing them through an 8-step pipeline, and selling curated datasets to AI companies.

---

## Scale Targets

| Phase | Users | Video Hours | Daily Ingestion | Total Storage |
|-------|-------|-------------|-----------------|---------------|
| 1 | 1,000 | 50K hrs | ~800 GB/day | ~20 TB |
| 2 | 10,000 | 500K hrs | ~8 TB/day | ~200 TB |
| 3 | 100,000+ | 5M+ hrs | ~80 TB/day | ~2 PB |

Assumptions: 1080p30 H.265 at ~400 MB/hr. Phase 2 assumes 10K users recording ~2 hrs/day.

---

## 1. Upload Pipeline

### 1.1 Architecture: S3 Presigned URL + Multipart Upload

The upload flow is **client-direct-to-bucket** -- the backend never proxies video bytes.

```
Mobile/Desktop Client
    |
    | 1. POST /api/uploads/init  (auth + metadata)
    v
API Server (ECS/Lambda)
    |
    | 2. CreateMultipartUpload -> returns uploadId + presigned URLs per part
    v
Client uploads parts directly to S3 (parallel, 5-64 MB chunks)
    |
    | 3. POST /api/uploads/complete  (ETags array)
    v
API Server calls CompleteMultipartUpload
    |
    | 4. S3 Event Notification -> SQS -> triggers processing pipeline
    v
```

**Key design decisions:**

- **Chunk size**: 32 MB default (balances retry cost vs overhead; configurable down to 5 MB on mobile)
- **Parallel uploads**: Client uploads 3-6 parts concurrently
- **S3 Transfer Acceleration**: Enabled for all buckets. Adds ~$0.04/GB but cuts upload time 50-300% for international users. Critical for mobile on cell networks.
- **Presigned URL expiration**: 60 minutes per part (generous for mobile; short enough for security)
- **Idempotency**: Client sends `X-Upload-Token` on init; server deduplicates to prevent double uploads on retry
- **Deterministic keys**: `{tenant_id}/{YYYY}/{MM}/{DD}/{uuid}.mp4` -- prevents S3 hot partitions, simplifies lifecycle rules
- **CORS**: Must expose `ETag` header in CORS config (required for multipart completion)
- **Checksums**: CRC32C checksum per part (S3 Signature V4 supports this natively)

### 1.2 Resumable Uploads: tus Protocol

For mobile and unreliable networks, layer the **tus protocol** (v1.0) on top of S3 multipart.

**Why tus over custom:**
- Standardized, battle-tested (Vimeo, Google use it)
- Client SDKs for iOS (TUSKit), Android (tus-android), JS (tus-js-client), React Native
- Built-in resume: client queries `HEAD` to check how much was uploaded, resumes from there
- No fixed chunk size requirement -- chunkless mode for WiFi, chunked for cellular
- Reference server `tusd` (Go) has S3 backend built-in

**Implementation:**
- Run `tusd` (Go binary) as a sidecar on ECS, backed by S3
- tusd handles multipart lifecycle: init, part uploads, resume, completion
- On completion, tusd fires a webhook to the API server
- API server records the upload in PostgreSQL and emits an SQS message

**Recommendation:** Use tus for mobile clients, raw S3 multipart for desktop (simpler, fewer moving parts).

### 1.3 Bandwidth Estimates

| Phase | Users | Hrs/Day | Daily Ingestion | Sustained Bandwidth |
|-------|-------|---------|-----------------|-------------------|
| 1 | 1,000 | 1 hr/user | 400 GB | ~37 Mbps |
| 2 | 10,000 | 2 hrs/user | 8 TB | ~740 Mbps |
| 3 | 100,000 | 2 hrs/user | 80 TB | ~7.4 Gbps |

At Phase 2 (8 TB/day), Transfer Acceleration is non-negotiable. At Phase 3, consider multi-region ingest endpoints (S3 replication across us-east-1, eu-west-1, ap-northeast-1).

### 1.4 Mobile Upload Strategy

- **Background uploads**: iOS `NSURLSession` background mode, Android `WorkManager`
- **Adaptive chunk size**: WiFi = 32 MB chunks; Cellular = 5 MB chunks; detect via network type API
- **Bandwidth throttling**: Cap at 80% of available bandwidth to avoid degrading user experience
- **Upload queue**: FIFO with priority (newest recordings first)
- **Offline resilience**: SQLite queue on device; uploads resume when connectivity returns
- **Progress persistence**: Part completion state saved to device storage; survives app kill

### 1.5 Storage Provider Cost Comparison

| Provider | Storage/TB/mo | Egress/TB | PUT/1M | GET/1M | Notes |
|----------|--------------|-----------|--------|--------|-------|
| **AWS S3** | $23.00 | $90.00 | $5.00 | $0.40 | Best ecosystem, most expensive |
| **Cloudflare R2** | $15.00 | **$0.00** | $4.50 | $0.36 | Zero egress -- ideal for buyer downloads |
| **Backblaze B2** | $6.00 | $10.00* | $4.00 | $0.40 | *Free up to 3x stored |
| **Wasabi** | $5.00 | **$0.00** | Free | Free | 90-day minimum retention, no delete-early |

**Recommended hybrid approach:**

| Use Case | Provider | Why |
|----------|----------|-----|
| **Ingest landing zone** | AWS S3 | Tight integration with ECS/Lambda/SQS processing pipeline |
| **Processed dataset storage** | Cloudflare R2 | Zero egress for buyer downloads; S3-compatible API |
| **Cold archive (raw originals)** | Wasabi or S3 Glacier Deep Archive | Cheapest long-term; raw footage rarely re-accessed |

**Cost at scale (Phase 2: 200 TB stored, 500 TB cumulative):**

| Strategy | Monthly Storage | Monthly Egress (50 TB buyer downloads) | Total |
|----------|----------------|----------------------------------------|-------|
| All S3 | $4,600 | $4,500 | **$9,100** |
| Hybrid (S3+R2+Wasabi) | $2,350 | $0 | **$2,350** |

The hybrid approach saves ~75% on storage+egress.

---

## 2. Processing Pipeline (8 Steps)

### Architecture: AWS Step Functions + ECS Fargate Spot

Step Functions orchestrates the DAG. Each step runs as an ECS Fargate Spot task (70% cheaper than on-demand). For GPU steps (face detection, CNN classifier), use EC2 Spot g4dn instances via AWS Batch.

```
S3 Upload Complete
    |
    v
SQS -> Step Functions Workflow
    |
    +--> Step 1: FFmpeg Transcode (ECS Fargate Spot)
    |        |
    +--> Step 2: Game Detection (AWS Batch, g4dn Spot)
    |        |
    +--> Step 3: Quality Scoring (ECS Fargate Spot)
    |        |
    +--> Step 4: Content Filtering (ECS Fargate Spot)
    |        |
    +--> Step 5: PII Scan (AWS Batch, g4dn Spot)
    |        |
    +--> Step 6: Input Log Alignment (ECS Fargate Spot)
    |        |
    +--> Step 7: Engine Metadata Merge (ECS Fargate Spot)
    |        |
    +--> Step 8: Write to Data Catalog (Lambda)
    v
Dataset Ready in R2
```

### Step-by-Step Breakdown

#### Step 1: FFmpeg Transcode

**Goal:** Normalize all inputs to 1080p/30fps H.265, constant bitrate.

| Attribute | Value |
|-----------|-------|
| **AWS Service** | ECS Fargate Spot (CPU-only) |
| **Instance** | 4 vCPU, 8 GB RAM per task |
| **Tool** | FFmpeg 6.x (static build in Docker) |
| **Command** | `ffmpeg -i input.mp4 -c:v libx265 -preset medium -crf 23 -vf "scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2,fps=30" -c:a aac -b:a 128k output.mp4` |
| **Processing time** | ~0.5-1.5x real-time (1 hr video = 30-90 min) |
| **Cost per hour of video** | ~$0.08-0.15 (Fargate Spot: 4 vCPU * $0.013/hr * 1.5 hrs) |
| **At 10K hrs/day** | ~$800-1,500/day |
| **Open-source** | FFmpeg (LGPL), HandBrake CLI (GPL) |

**Optimization:** For Phase 3, switch to c5.4xlarge EC2 Spot ($0.068/hr Spot vs $0.68 on-demand) with FFmpeg `-threads 16` for 4-6x real-time encoding.

#### Step 2: Game Detection (CNN Classifier)

**Goal:** Classify which game is being played from UI frames.

| Attribute | Value |
|-----------|-------|
| **AWS Service** | AWS Batch on g4dn.xlarge Spot ($0.16/hr Spot) |
| **Model** | EfficientNet-B4 or DenseNet-121 fine-tuned on game screenshots |
| **Approach** | Sample 1 frame every 10 seconds -> classify -> majority vote per segment |
| **Training data** | Scrape game screenshot databases + manual labeling (1K images per game, 100+ games) |
| **Processing time** | ~5 min per hour of video (sampling 360 frames, batch inference) |
| **Cost per hour of video** | ~$0.013 |
| **At 10K hrs/day** | ~$130/day |
| **Open-source** | PyTorch/TorchVision, timm (PyTorch Image Models), EfficientNet pretrained weights |

**Alternative:** Start with a simpler approach -- OCR on title screens + audio fingerprinting for game music. CNN is the long-term play.

#### Step 3: Quality Scoring

**Goal:** Score each recording 0-100 on usefulness for AI training.

| Attribute | Value |
|-----------|-------|
| **AWS Service** | ECS Fargate Spot (CPU) |
| **Tool** | Custom Python scorer using OpenCV + numpy |
| **Processing time** | ~3-5 min per hour of video |
| **Cost per hour of video** | ~$0.005 |
| **At 10K hrs/day** | ~$50/day |

**Quality Scoring Algorithm (detailed in Section 5):**

```python
quality_score = (
    action_density_score * 0.30 +     # Movement + inputs per minute
    content_uniqueness_score * 0.20 +  # Frame diversity via perceptual hashing
    session_length_score * 0.15 +      # Minimum 5 min, bonus for 15-60 min
    input_richness_score * 0.15 +      # Variety of input types
    fps_stability_score * 0.10 +       # Consistent frame timing
    resolution_score * 0.10            # Native resolution quality
)
```

#### Step 4: Content Filtering (Auto-Trim)

**Goal:** Detect and remove death screens, menus, loading screens, cutscenes.

| Attribute | Value |
|-----------|-------|
| **AWS Service** | ECS Fargate Spot (CPU) |
| **Approach** | Frame differencing + template matching + scene change detection |
| **Tool** | OpenCV (scene detection), PySceneDetect, custom classifiers |
| **Method** | Compute optical flow between frames; near-zero flow = static screen (menu/loading/death). Train lightweight MobileNetV2 classifier on labeled menu/loading/death screen examples |
| **Processing time** | ~5-8 min per hour of video |
| **Cost per hour of video** | ~$0.01 |
| **At 10K hrs/day** | ~$100/day |
| **Open-source** | PySceneDetect (BSD), OpenCV, MobileNetV2 (TorchVision) |

**Output:** Trimmed video + trim manifest (JSON with kept/removed segments and reasons).

#### Step 5: PII Scan

**Goal:** Detect and blur faces, usernames, personal information in video.

| Attribute | Value |
|-----------|-------|
| **AWS Service** | AWS Batch on g4dn.xlarge Spot (GPU required for real-time face detection) |
| **Tool** | `deface` (CLI tool, CenterFace model) for face blurring; PaddleOCR for text detection |
| **Approach** | 1) Run face detection on every Nth frame (N=5 for 30fps = 6 checks/sec). 2) Run OCR on detected text regions, match against username patterns. 3) Apply Gaussian blur to detected regions, interpolating positions between sampled frames |
| **Processing time** | ~15-30 min per hour of video (GPU-accelerated) |
| **Cost per hour of video** | ~$0.04-0.08 |
| **At 10K hrs/day** | ~$400-800/day |
| **Open-source** | deface (MIT), DeepFace (MIT), YOLO-Face, PaddleOCR (Apache 2.0), CenterFace |

**Privacy levels:**
- Level 1 (default): Blur faces only
- Level 2: Blur faces + detected usernames/gamertags
- Level 3: Blur all text overlays

#### Step 6: Input Log Alignment

**Goal:** Synchronize keyboard/mouse/controller events to video frames.

| Attribute | Value |
|-----------|-------|
| **AWS Service** | ECS Fargate Spot (CPU-only, lightweight) |
| **Approach** | Timestamp correlation using shared clock reference |
| **Method** | 1) Client records inputs with high-precision timestamps (monotonic clock). 2) Client records a sync pulse at recording start (e.g., specific key combo) visible in both video and input log. 3) Server aligns using cross-correlation of event density with visual activity. 4) Output: per-frame input state array |
| **Format** | Apache Parquet -- one row per frame with columns: `frame_idx, timestamp_ms, keys_pressed[], mouse_x, mouse_y, mouse_buttons, controller_axes[], controller_buttons[]` |
| **Processing time** | ~1-2 min per hour of video |
| **Cost per hour of video** | ~$0.002 |
| **At 10K hrs/day** | ~$20/day |
| **Open-source** | pandas, pyarrow (Parquet), numpy |

#### Step 7: Engine Metadata Merge

**Goal:** Merge game engine telemetry (if available) with video data.

| Attribute | Value |
|-----------|-------|
| **AWS Service** | ECS Fargate Spot (CPU) |
| **Data sources** | Game engine replay files, mod APIs, memory-mapped state (advanced) |
| **Format** | Parquet file with per-frame game state: `frame_idx, player_x, player_y, player_z, health, inventory[], npc_positions[], game_event` |
| **Processing time** | ~1-3 min per hour (parsing engine-specific formats) |
| **Cost per hour of video** | ~$0.003 |
| **At 10K hrs/day** | ~$30/day |

**Note:** This step is opt-in. Most recordings will not have engine metadata initially. Build the schema and pipeline now; populate as game integrations ship.

#### Step 8: Write to Data Catalog

**Goal:** Register the processed dataset in the catalog with full metadata.

| Attribute | Value |
|-----------|-------|
| **AWS Service** | Lambda (< 1 sec execution) |
| **Action** | Write metadata record to PostgreSQL + update search index (Typesense/Meilisearch) + copy processed files to R2 |
| **Catalog entry** | `session_id, user_id, game_id, game_title, duration_seconds, quality_score, resolution, fps, has_input_log, has_engine_metadata, trim_manifest_url, video_url, input_log_url, metadata_url, created_at, tags[]` |
| **Cost per hour of video** | ~$0.0001 |
| **Open-source** | PostgreSQL, Typesense (MIT) or Meilisearch (MIT) |

### Processing Pipeline Cost Summary (Phase 2: 10K hrs/day)

| Step | Daily Cost | Monthly Cost |
|------|-----------|-------------|
| 1. FFmpeg Transcode | $1,200 | $36,000 |
| 2. Game Detection | $130 | $3,900 |
| 3. Quality Scoring | $50 | $1,500 |
| 4. Content Filtering | $100 | $3,000 |
| 5. PII Scan | $600 | $18,000 |
| 6. Input Log Alignment | $20 | $600 |
| 7. Engine Metadata Merge | $30 | $900 |
| 8. Data Catalog Write | $1 | $30 |
| Step Functions orchestration | $25 | $750 |
| **Total** | **~$2,156** | **~$64,680** |

**Cost per hour of processed video: ~$0.22**

---

## 3. Storage Architecture

### 3.1 Object Storage (Video + Datasets)

```
Ingest Bucket (S3, us-east-1)
    |-- raw/{tenant_id}/{YYYY}/{MM}/{DD}/{uuid}.mp4
    |-- Lifecycle: move to Glacier Deep Archive after 30 days
    |
Processed Bucket (Cloudflare R2)  <-- zero egress for buyers
    |-- processed/{game_id}/{session_id}/video.mp4
    |-- processed/{game_id}/{session_id}/input_log.parquet
    |-- processed/{game_id}/{session_id}/metadata.json
    |-- processed/{game_id}/{session_id}/trim_manifest.json
    |-- processed/{game_id}/{session_id}/engine_state.parquet  (if available)
    |
Archive Bucket (Wasabi or S3 Glacier Deep Archive)
    |-- archive/{tenant_id}/{session_id}.tar.zst
    |-- All original raw footage, compressed
```

### 3.2 Storage Cost Projections

| Phase | Raw Storage | Processed Storage | Archive | Monthly Cost (Hybrid) |
|-------|------------|-------------------|---------|----------------------|
| 1 (50K hrs) | 20 TB | 15 TB | 20 TB | ~$500 |
| 2 (500K hrs) | 200 TB | 150 TB | 200 TB | ~$3,500 |
| 3 (5M hrs) | 2 PB | 1.5 PB | 2 PB | ~$25,000 |

Breakdown at Phase 2:
- S3 ingest (hot, 30-day window): ~50 TB * $23/TB = $1,150
- R2 processed: 150 TB * $15/TB = $2,250
- Wasabi archive: 200 TB * $5/TB = $1,000 (but 90-day min retention is fine for archive)
- S3 Glacier Deep Archive alternative: 200 TB * $1/TB = $200

### 3.3 S3 Lifecycle Policies

```json
{
  "Rules": [
    {
      "ID": "raw-to-glacier",
      "Filter": {"Prefix": "raw/"},
      "Transitions": [
        {"Days": 7, "StorageClass": "STANDARD_IA"},
        {"Days": 30, "StorageClass": "GLACIER_IR"},
        {"Days": 90, "StorageClass": "DEEP_ARCHIVE"}
      ]
    },
    {
      "ID": "abort-incomplete-multipart",
      "Filter": {},
      "AbortIncompleteMultipartUpload": {"DaysAfterInitiation": 3}
    }
  ]
}
```

### 3.4 Metadata Database: PostgreSQL

PostgreSQL handles:
- User accounts, sessions, uploads
- Processing pipeline state (which step each recording is on)
- Dataset catalog metadata
- Buyer accounts, purchases, access tokens
- Bounty system

**Why PostgreSQL:**
- JSONB for flexible metadata
- Full-text search for dataset discovery
- Row-level security for multi-tenant isolation
- Managed via Amazon RDS or Neon (serverless)

**Schema highlights:**

```sql
-- Core tables
sessions (id, user_id, game_id, upload_status, processing_status,
          quality_score, duration_seconds, created_at)
processing_steps (session_id, step_name, status, started_at,
                  completed_at, error_message, output_metadata JSONB)
datasets (id, name, description, game_ids[], total_hours,
          total_sessions, price_per_hour, created_at)
dataset_sessions (dataset_id, session_id)  -- many-to-many
purchases (id, buyer_id, dataset_id, amount_cents, stripe_payment_id,
           access_expires_at)
bounties (id, buyer_id, game_id, scenario_description,
          reward_per_hour_cents, min_quality_score, status)
```

### 3.5 Analytics Database: ClickHouse

ClickHouse for high-volume analytical queries (dashboards, usage analytics, buyer analytics).

**Why ClickHouse over TimescaleDB:**
- 6-7x faster on large aggregations (billions of rows)
- 30-50% lower storage costs (aggressive compression)
- Column-oriented -- perfect for "scan all sessions where game=X and quality>80"
- At Phase 2 scale (500K sessions), ClickHouse shines

**What goes in ClickHouse:**
- Upload events (timestamp, user_id, file_size, upload_duration)
- Processing metrics (step timings, costs, error rates)
- Quality score distributions
- Buyer download patterns
- Revenue analytics

**Deployment:** ClickHouse Cloud (managed) or self-hosted on 2x c5.2xlarge (~$300/mo).

### 3.6 Search Index: Typesense

For the buyer-facing dataset search API:
- Full-text search across game titles, tags, descriptions
- Faceted filtering (game, quality range, duration, has_input_log, has_engine_metadata)
- Geo-search if needed (player regions)
- Sub-10ms query latency
- Sync from PostgreSQL via CDC (Debezium) or scheduled sync

**Why Typesense over Elasticsearch:**
- Simpler to operate (single binary)
- Lower resource usage
- Built-in typo tolerance
- Open-source (GPL-3)

---

## 4. Buyer API

### 4.1 REST API Design

```
Base URL: https://api.gamedatalabs.com/v1

Authentication: Bearer token (API key) per buyer organization

Endpoints:

# Discovery
GET    /datasets                    # List/search datasets
GET    /datasets/{id}               # Dataset details
GET    /datasets/{id}/sessions      # List sessions in dataset
GET    /datasets/{id}/preview       # 30-second preview clips
GET    /games                       # List available games
GET    /games/{id}/stats            # Stats per game

# Download
POST   /datasets/{id}/download      # Initiate bulk download (returns signed URLs)
GET    /downloads/{job_id}/status   # Check download preparation status
GET    /sessions/{id}/video         # Stream/download single session video
GET    /sessions/{id}/input-log     # Download input log (Parquet)
GET    /sessions/{id}/metadata      # Download metadata (JSON)
GET    /sessions/{id}/frames        # Download as frame sequence (JPEG/PNG tar)
GET    /sessions/{id}/state-actions # Download state-action pairs (Parquet)

# Bounties
POST   /bounties                    # Create bounty request
GET    /bounties                    # List buyer's bounties
GET    /bounties/{id}               # Bounty status + matching sessions
PATCH  /bounties/{id}               # Update bounty parameters

# Billing
GET    /usage                       # Current billing period usage
GET    /invoices                    # Past invoices
```

### 4.2 Dataset Format Options

Buyers can request data in multiple formats:

| Format | Use Case | Contents |
|--------|----------|----------|
| **Video clips** | Visual AI training, imitation learning | H.265 MP4, trimmed to gameplay only |
| **Frame sequences** | Computer vision, object detection | JPEG frames at configurable FPS (1, 5, 10, 30) |
| **State-action pairs** | Reinforcement learning, behavior cloning | Parquet: `frame_idx, game_state{}, action{}` |
| **Input replay** | Input prediction models | Parquet: per-frame keyboard/mouse/controller state |
| **Multi-modal bundle** | Full training pipeline | Video + input log + metadata + engine state (if avail) |

Frame sequence export is done on-demand via a Lambda triggered by the download request. Uses FFmpeg to extract frames at requested FPS, packages into tar.gz, uploads to R2, returns signed URL.

### 4.3 Bulk Download via S3 Transfer Acceleration

For large dataset downloads (100+ GB):
1. Buyer requests download via `POST /datasets/{id}/download`
2. Backend prepares a manifest of all files
3. Returns an array of presigned R2 URLs (R2 is S3-compatible)
4. Buyer uses `aws s3 cp --recursive` or any S3-compatible tool
5. R2 zero-egress means download costs = $0 regardless of volume

For extremely large transfers (10+ TB), offer **S3-to-S3 direct transfer** -- buyer provides their S3 bucket, we do a server-side copy (requires cross-account IAM role).

### 4.4 Stripe Billing Integration

**Pricing models:**

| Tier | Monthly Fee | Included Hours | Overage/Hour |
|------|------------|----------------|--------------|
| Starter | $499 | 100 hrs | $3.00 |
| Pro | $2,499 | 1,000 hrs | $2.00 |
| Enterprise | Custom | Custom | Custom |

**Stripe implementation:**
- **Meters API** for usage tracking: each dataset download = meter event with hours consumed
- **Subscriptions** with usage-based overage billing
- **Credits system**: Prepaid credit packages for burst usage (Stripe credit grants)
- **Webhook** on `invoice.payment_succeeded` to extend access tokens

```
Stripe Meters --> Usage Events (hours downloaded)
    |
    v
Stripe Subscription (base plan + metered overage)
    |
    v
Invoice generated monthly --> Webhook to our API --> Access control
```

### 4.5 Bounty System

Buyers post bounties for specific data they need:

```json
{
  "game_id": "valorant",
  "scenario": "clutch rounds (1v3 or higher) with win",
  "min_quality_score": 75,
  "reward_per_hour_usd": 5.00,
  "max_hours": 500,
  "deadline": "2026-06-01"
}
```

- Bounties show in the recorder app for eligible users
- When a recording matches bounty criteria (game + quality + scenario tag), user gets bonus payout
- Scenario matching starts simple (game + quality threshold) and evolves to ML-based scenario detection

---

## 5. Quality Scoring Algorithm

### 5.1 Component Scores (0-100 each)

#### Action Density Score (weight: 0.30)

Measures movement and input frequency per minute.

```python
def action_density_score(input_log, video_frames):
    # Count distinct inputs per minute
    inputs_per_min = count_inputs_per_minute(input_log)

    # Count significant pixel changes per minute (optical flow magnitude)
    motion_per_min = compute_optical_flow_magnitude(video_frames, sample_rate=2)

    # Combine: high input + high motion = high action
    raw_score = 0.6 * normalize(inputs_per_min, min=10, max=200) + \
                0.4 * normalize(motion_per_min, min=1000, max=50000)

    # Penalize extreme spikes (likely macro/bot)
    if coefficient_of_variation(inputs_per_min) > 3.0:
        raw_score *= 0.5  # suspicious regularity

    return clamp(raw_score * 100, 0, 100)
```

#### Content Uniqueness Score (weight: 0.20)

Detects repetitive content via perceptual hashing.

```python
def content_uniqueness_score(video_frames):
    # Sample 1 frame per second
    hashes = [perceptual_hash(frame) for frame in sample_frames(fps=1)]

    # Compute pairwise Hamming distances
    unique_segments = count_segments_with_low_similarity(
        hashes, threshold=8, window=30  # 30-second windows
    )
    total_segments = len(hashes) // 30

    return (unique_segments / total_segments) * 100
```

#### Session Length Score (weight: 0.15)

```python
def session_length_score(duration_minutes):
    if duration_minutes < 5:
        return 0  # Too short, reject
    elif duration_minutes < 15:
        return 30 + (duration_minutes - 5) * 4  # 30-70
    elif duration_minutes <= 60:
        return 70 + (duration_minutes - 15) * 0.67  # 70-100
    elif duration_minutes <= 120:
        return 100  # Sweet spot
    else:
        return max(80, 100 - (duration_minutes - 120) * 0.1)  # Slight decay for very long
```

#### Input Richness Score (weight: 0.15)

```python
def input_richness_score(input_log):
    # Count unique input combinations used
    unique_combos = count_unique_input_combinations(input_log)

    # Count input type diversity (keyboard, mouse movement, mouse clicks, controller)
    input_types_used = count_input_types(input_log)

    # Measure input timing variance (non-robotic)
    timing_entropy = compute_timing_entropy(input_log)

    return (
        normalize(unique_combos, min=5, max=100) * 40 +
        normalize(input_types_used, min=1, max=4) * 30 +
        normalize(timing_entropy, min=1.0, max=4.0) * 30
    )
```

#### FPS Stability Score (weight: 0.10)

```python
def fps_stability_score(frame_timestamps):
    expected_interval = 1.0 / 30  # 33.3ms for 30fps
    actual_intervals = np.diff(frame_timestamps)

    # Percentage of frames within 20% of expected interval
    stable_frames = np.sum(np.abs(actual_intervals - expected_interval) <
                          expected_interval * 0.2)
    stability_ratio = stable_frames / len(actual_intervals)

    return stability_ratio * 100
```

#### Resolution Score (weight: 0.10)

```python
def resolution_score(width, height):
    pixels = width * height
    if pixels >= 1920 * 1080:
        return 100
    elif pixels >= 1280 * 720:
        return 70
    elif pixels >= 854 * 480:
        return 40
    else:
        return 10  # Very low quality
```

### 5.2 Content Detection (Menus, Death Screens, Loading, Cutscenes)

| Content Type | Detection Method | Action |
|-------------|-----------------|--------|
| **Loading screens** | Near-zero optical flow + static progress indicators (template match) | Auto-trim |
| **Menus** | Near-zero optical flow + high text density (OCR) + cursor movement without gameplay motion | Auto-trim |
| **Death screens** | Sudden motion stop + screen darkening/reddening + specific UI patterns | Trim (keep 3 sec before) |
| **Cutscenes** | Zero input activity + high visual motion (pre-rendered) + letterboxing detection | Trim or flag |
| **Idle time** | Zero input for >30 seconds + minimal screen change | Auto-trim |
| **AFK** | Zero input for >2 minutes | Split session |

**Implementation:** Train a lightweight MobileNetV2 classifier on 5 classes (gameplay, menu, loading, death, cutscene) with ~5K labeled frames per class. Run inference on 1 frame/sec. Post-process with minimum segment duration (no 1-frame cuts).

---

## 6. Cost Optimization

### 6.1 Compute Optimization

| Strategy | Savings | Implementation |
|----------|---------|---------------|
| **EC2 Spot for processing** | 70-90% off on-demand | AWS Batch with Spot fleet; diversify across c5, c5a, c6i instance types |
| **Fargate Spot for lightweight steps** | 70% off Fargate | Steps 3, 4, 6, 7 on Fargate Spot; tolerate 2-min interruption |
| **Right-sizing** | 20-40% | Profile each step; Step 6 needs 1 vCPU, Step 1 needs 4 vCPU |
| **ARM instances (Graviton)** | 20% cheaper + faster | FFmpeg on c6g/c7g; 20% better price-performance |
| **Reserved capacity for baseline** | 30-60% off | Reserve 40% of peak processing for steady-state load |
| **Batch processing windows** | 10-20% Spot savings | Process uploads in off-peak hours (2 AM - 8 AM) when Spot prices are lowest |

### 6.2 Storage Optimization

| Strategy | Savings | Implementation |
|----------|---------|---------------|
| **S3 Intelligent-Tiering** | Auto-optimized | For ingest bucket; auto-moves to IA/Archive tiers |
| **Lifecycle to Glacier Deep Archive** | 95% vs Standard | Raw footage after 90 days -> $1/TB/mo |
| **R2 for egress-heavy workloads** | 100% egress savings | All buyer-facing data on R2 |
| **Abort incomplete multipart** | Prevents waste | 3-day policy on all buckets |
| **Zstandard compression for archives** | 30-50% size reduction | `tar -cf - session/ | zstd -19 > archive.tar.zst` |

### 6.3 CDN for Buyer Downloads

- **CloudFront** in front of R2 (yes, this works via R2's S3-compatible API)
- Cache popular datasets at edge (many buyers download the same datasets)
- CloudFront pricing: $0.085/GB (but R2 egress is already free, so CloudFront only helps with latency/caching)
- **Alternative:** Cloudflare CDN natively in front of R2 = truly $0 delivery cost

### 6.4 Phase-by-Phase Cost Estimates

#### Phase 1 (1,000 users, 50K hours total)

| Category | Monthly Cost |
|----------|-------------|
| Compute (processing) | $3,300 |
| Storage (hybrid S3+R2+archive) | $500 |
| Database (RDS + ClickHouse) | $400 |
| Networking (Transfer Acceleration) | $800 |
| Step Functions | $50 |
| Search (Typesense) | $50 |
| **Total** | **~$5,100/mo** |

#### Phase 2 (10,000 users, 500K hours, 10K hrs/day processing)

| Category | Monthly Cost |
|----------|-------------|
| Compute (processing pipeline) | $64,700 |
| Storage (hybrid) | $3,500 |
| Database (RDS + ClickHouse) | $1,500 |
| Networking (Transfer Acceleration) | $10,000 |
| Step Functions | $750 |
| Search (Typesense) | $200 |
| API servers (ECS) | $800 |
| **Total** | **~$81,450/mo** |

**Cost per hour of video (fully loaded): ~$0.27**
**Breakeven: sell data at >$0.27/hr to be gross-margin positive**

#### Phase 3 (100K users, 5M hours)

| Category | Monthly Cost |
|----------|-------------|
| Compute (processing) | $500,000 |
| Storage (hybrid) | $25,000 |
| Database cluster | $10,000 |
| Networking | $80,000 |
| Orchestration + API | $5,000 |
| **Total** | **~$620,000/mo** |

At this scale, negotiate AWS Enterprise Discount Program (EDP) for 15-25% off all services. Also consider dedicated hosts for FFmpeg processing.

---

## 7. Infrastructure Summary

### Technology Stack

| Layer | Technology | Why |
|-------|-----------|-----|
| **API** | FastAPI on ECS Fargate | Async, fast, auto-docs, Python ecosystem |
| **Auth** | Clerk or Auth0 (users) + API keys (buyers) | Don't build auth |
| **Queue** | Amazon SQS | Decouples upload from processing |
| **Orchestration** | AWS Step Functions | Visual workflow, built-in retry/error handling |
| **Processing (CPU)** | ECS Fargate Spot | Serverless containers, no cluster management |
| **Processing (GPU)** | AWS Batch on g4dn Spot | GPU inference for CV models |
| **Object Storage** | S3 (ingest) + R2 (serve) + Wasabi (archive) | Cost-optimized per access pattern |
| **Metadata DB** | PostgreSQL (RDS) | Relational, JSONB, battle-tested |
| **Analytics DB** | ClickHouse Cloud | Fast aggregations, column-oriented |
| **Search** | Typesense | Fast, simple, typo-tolerant |
| **Billing** | Stripe (Meters API + Subscriptions) | Usage-based billing out of the box |
| **CDN** | Cloudflare (in front of R2) | Zero-cost delivery |
| **Monitoring** | Datadog or Grafana Cloud | Traces, metrics, logs in one place |
| **CI/CD** | GitHub Actions + Terraform | Infrastructure as code |

### System Diagram

```
                    +---------+
                    |  Users  |
                    | (Mobile |
                    | Desktop)|
                    +----+----+
                         |
              tus / S3 Multipart
              (Transfer Acceleration)
                         |
                    +----v----+
                    |   S3    |
                    | (Ingest)|
                    +----+----+
                         |
                    S3 Event -> SQS
                         |
                +--------v--------+
                |  Step Functions  |
                |   Orchestrator   |
                +--------+--------+
                         |
        +-------+--------+--------+-------+
        |       |        |        |       |
     Transcode Game   Quality  Content  PII
     (Fargate) Detect  Score   Filter  Scan
               (Batch)                 (Batch)
        |       |        |        |       |
        +-------+--------+--------+-------+
                         |
                   Input Align
                   Metadata Merge
                   Catalog Write
                         |
                +--------v--------+
                |   Cloudflare R2  |
                | (Processed Data) |
                +--------+--------+
                         |
              Cloudflare CDN (zero egress)
                         |
                +--------v--------+
                |   Buyer API     |
                |   (FastAPI/ECS) |
                +--------+--------+
                    |         |
              +-----+    +---+-----+
              |Stripe|    |Typesense|
              |Billing|   | Search  |
              +------+    +--------+
                    |
            +-------+-------+
            | PostgreSQL    |
            | (RDS)         |
            +-------+-------+
                    |
            +-------v-------+
            | ClickHouse    |
            | (Analytics)   |
            +---------------+
```

### Key Open-Source Tools Summary

| Tool | Purpose | License |
|------|---------|---------|
| FFmpeg | Video transcoding | LGPL |
| PySceneDetect | Scene/shot detection | BSD |
| OpenCV | Computer vision primitives | Apache 2.0 |
| EfficientNet/DenseNet (timm) | Game classification CNN | Apache 2.0 |
| deface | Face anonymization | MIT |
| PaddleOCR | Text detection/recognition | Apache 2.0 |
| MobileNetV2 | Lightweight classification | Apache 2.0 |
| Typesense | Search engine | GPL-3 |
| tusd | tus protocol server | MIT |
| ClickHouse | Analytics database | Apache 2.0 |

---

## 8. Recommended Implementation Order

### Phase 1 Sprint Plan

| Sprint | Focus | Deliverable |
|--------|-------|-------------|
| 1-2 | Upload pipeline | S3 multipart + presigned URLs + basic API |
| 3-4 | Steps 1-2 | FFmpeg transcode + game detection MVP |
| 5-6 | Steps 3-5 | Quality scoring + content filtering + PII scan |
| 7-8 | Steps 6-8 | Input alignment + catalog + search |
| 9-10 | Buyer API | REST API + Stripe integration + R2 delivery |
| 11-12 | Mobile SDK | tus upload + background recording SDK |

### Critical Path Dependencies

```
Upload Pipeline (Sprint 1-2)
    |
    +--> FFmpeg Transcode (Sprint 3) -- blocks all downstream steps
    |
    +--> Game Detection (Sprint 4) -- blocks dataset organization
    |
    +--> Quality Scoring (Sprint 5) -- blocks dataset curation
    |
    +--> PII Scan (Sprint 6) -- blocks any data delivery to buyers
    |
    +--> Buyer API (Sprint 9) -- needs all processing steps complete
```

PII Scan is the legal blocker -- no data leaves the platform until PII scanning is production-grade and audited.
