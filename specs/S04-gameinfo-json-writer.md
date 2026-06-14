---
task_id: S04-gameinfo-json-writer
project: gamedata-recorder
priority: 0
estimated_minutes: 40
modifies: ["src/record/local_recording.rs", "crates/constants/src/lib.rs", "tests in same crate"]
executor: opencode
---

## 目标（PRD R5.1, P0, 买方合同）
每个 session 在 recording_location 下额外写出 `gameinfo.json`（新 sidecar 文件，与现有 metadata.json 并排），不替换、不改动任何现有文件格式。

## Schema（买方合同，字段名/类型不可改）
```json
{
  "game_title": "<string>",
  "game_version": "<string|null>",
  "session_id": "<string>",
  "route_type": "<int 1|2|3>",
  "started_at": "<RFC3339 UTC string>",
  "duration_s": "<float>"
}
```

## 实现锚点
- 在 `src/record/local_recording.rs` 的 `write_metadata_and_validate()` 里，**写完 metadata.json 之后**追加写 gameinfo.json（同一函数已持有全部数据源）：
  - `game_title` ← `game_exe`（去 `.exe` 后缀；若有 `window_name` 用它更佳）
  - `game_version` ← 从 `window_name` 尽力解析版本号（如 "Minecraft* 1.21.4" → "1.21.4"），解析不出则 `null`
  - `session_id` ← 复用现有 metadata 用的同一 session id（不要新生成；从现有 metadata 构造路径里取）
  - `route_type` ← 入参 `route_type: Option<u8>`；**为 None 时不写 gameinfo.json**（route_type 是必填 1|2|3，未打标的 clip 不产出 gameinfo，和 metadata.json 的 route_type 省略逻辑一致）
  - `started_at` ← `start_time` 转 RFC3339 UTC
  - `duration_s` ← 已算好的 `duration`
- 文件名常量加到 `crates/constants/src/lib.rs` 的 `filename::recording` 模块（如 `GAMEINFO`）
- **原子写**：temp-file + rename（和 R5.6 一致），别半写

## 验收标准
- [ ] route_type=Some(2) 时产出合法 gameinfo.json，6 字段齐全且类型正确
- [ ] route_type=None 时**不**产出 gameinfo.json
- [ ] 新增单测覆盖上述两条 + version 解析（有版本/无版本两例）
- [ ] 现有 metadata.json 输出**逐字节不变**（加断言或对比测试）
- [ ] 本地 `cargo fmt` 通过（build 由 CI 验，mac 无法交叉编译 msvc）
- [ ] 先 `git checkout -b feat/gameinfo-json-r5.1 origin/main`，提交但**不 push**（我来 push+开 PR）

## 不要做
- 不改 metadata.json / LEM schema / 任何现有序列化字段
- 不加新依赖（chrono 若已在 deps 可用，否则用现有时间格式化手段）
- 不生成新 session_id、不动 UI、不要询问
