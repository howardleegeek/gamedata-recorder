---
task_id: S05-backend-tracker-upload-contract
project: gamedata-recorder
priority: 0
estimated_minutes: 50
modifies: ["backend/main.py", "backend/test_*.py"]
executor: opencode
---

## 目标
让 `backend/` FastAPI 暴露 recorder 客户端**实际调用**的上传端点,使"录完直接上传"端到端打通。recorder 是契约真源(已发布的 Windows 客户端),后端必须对齐它,不能反过来。

## 背景(已实证)
- recorder 默认 API base = `https://api.gamedatalabs.com`（src/api/mod.rs:13，env `GAMEDATA_API_URL` 可覆盖）
- recorder **实际调用**的路径(src/api/multipart_upload.rs, user_upload.rs):
  - `POST /tracker/upload/game_control/multipart/init`
  - `POST /tracker/upload/game_control/multipart/chunk`
  - `POST /tracker/upload/game_control/multipart/complete`
  - `POST /tracker/upload/game_control/multipart/abort/{upload_id}`
- 后端现有 `/api/v1/upload/{init,chunk,complete}` 路径**不被 recorder 调用**(死代码),但其 S3 presigned + 分块逻辑可直接复用
- 字段 schema 两边已匹配(total_size_bytes / chunk_size_bytes / game_control_id / game_exe / additional_metadata / chunk_number / chunk_hash)

## 实现(只改 backend/，不碰 recorder Rust 代码)
1. **先读 recorder 客户端确定精确契约**:`src/api/multipart_upload.rs` 和 `src/api/user_upload.rs` 里每个请求的 request body 字段、响应里读取的字段(`init_response.upload_id/game_control_id/total_chunks/chunk_size_bytes/expires_at`、chunk 的 etag、complete/abort 的形态)。**以 recorder 读取的字段为准**。
2. 在 backend/main.py 新增 4 个端点,路径与 recorder 完全一致,**复用现有 `/api/v1/upload/*` 的 S3 presigned/分块/DB 逻辑**(抽公共函数,不要复制粘贴两份):
   - `/tracker/upload/game_control/multipart/init` → 返回 recorder 期望的全部字段(upload_id, game_control_id, total_chunks, chunk_size_bytes, expires_at, 以及每 chunk 的 presigned url 若 recorder 用)
   - `/tracker/upload/game_control/multipart/chunk` → 返回 etag
   - `/tracker/upload/game_control/multipart/complete`
   - `/tracker/upload/game_control/multipart/abort/{upload_id}`
3. 保留现有 `/api/v1/upload/*`(向后兼容,别删)
4. 鉴权:沿用现有 auth 依赖(Bearer token,和 `/api/v1/user/info` 一致)
5. **生产硬化**(no-prototype 铁律):请求体大小上限已有(MAX_UPLOAD_BYTES)保持;新端点同样走 S3 超时配置;错误返回结构化 JSON 不 500 泄栈;无 S3 凭证时本地存储兜底路径也要覆盖到新端点

## 验收标准
- [ ] `pytest backend/` 全绿,新增测试覆盖 4 个 tracker 端点的 happy path + 鉴权失败 + 超大 reject
- [ ] 契约测试:构造 recorder init/chunk/complete 的真实请求体,断言响应含 recorder 代码读取的每个字段
- [ ] 现有 `/api/v1/upload/*` 测试仍绿(未回归)
- [ ] `black --check backend/` 通过
- [ ] 先 `git checkout -b feat/backend-tracker-upload-contract origin/main`,提交但**不 push**

## 不要做
- 不改任何 recorder Rust 代码(src/、crates/)
- 不删 `/api/v1/upload/*`
- 不加重型新依赖(boto3/fastapi/sqlalchemy 已在)
- 不动 DB schema 除非 recorder 契约确实需要新字段(需要则用 alembic 迁移,不裸改表)
- 不要询问,直接完成
