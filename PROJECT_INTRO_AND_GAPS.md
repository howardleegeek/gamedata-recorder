# GameData Recorder — Project Brief + Top 10 Open Tasks

*2026-05-12 · For: incoming engineer · From: Howard*

---

## 项目是什么

**GameData Recorder** 是一个 Windows 桌面客户端，自动捕获 PC 玩家的游戏画面、键鼠输入流、以及游戏引擎内部数据（坐标、旋转、FOV），上传到我们后端，经清洗后**卖给训练世界模型（World Model）与具身智能（Embodied AI）的 AI 公司**。

**用户端价值**：玩家正常玩游戏，自动赚钱（约 $10–$40/月，含引擎元数据可达 $40–$160/月）。
**买家端价值**：AI 公司拿到带"动作—画面—位置"对齐的高质量训练数据，比纯视频数据贵 2–4×。
**商业模型**：基于 OWL Control（Overworld AI，MIT 协议）派生，做生产化 + buyer schema 兼容 + 引擎 hook。

**当前阶段**：v2.6.0（2026-04-22），灰度阶段。代码库 ~15 k LOC Rust 客户端 + Python FastAPI 后端。
**仓库**：https://github.com/howardleegeek/gamedata-recorder
**入门必读**：仓库根目录的 `README.md` → `ONBOARDING.md` → `PRD_REQUIREMENTS.md` → `BUGS.md` → `TRIAGE.md`

**我们诚实的状态**：PRD 93 条需求中 P0 部分命中 ~75%（42/56），剩 25% 是项目里**最难、最有价值**的一部分。下面列出 10 条具体未完成的工作，按从最容易到最有挑战梯度排，欢迎挑一条。

---

## 我们目前差的 10 件事

| # | 任务 | 估时 | 难度 | 价值 | 入口 |
|---|---|---|---|---|---|
| 1 | **mpsc capacity 10 → 10,000**（防 stop-stall 时输入丢失） | 0.2 h | ⭐ | 🟢 | `crates/input-capture/src/lib.rs:83`，[`TRIAGE.md` Gate A4](TRIAGE.md) |
| 2 | **NVENC 探测在 Win N/LTSC 失败**（改 wmic 为 DXGI adapter 枚举） | 1 h | ⭐⭐ | 🟡 | `src/config.rs:15-22`，[`TRIAGE.md` Gate A2](TRIAGE.md) |
| 3 | **JSONL → CSV 重构 bug**（input_stats 多参数事件归零） | 1 h | ⭐⭐ | 🟡 | `src/validation/mod.rs:159-173`，[`TRIAGE.md` Gate A1](TRIAGE.md) |
| 4 | **200ms sleep 阻塞 tokio**（换成 `Notify` / oneshot） | 1.5 h | ⭐⭐⭐ | 🟡 | `src/record/obs_embedded_recorder.rs:690`，[`TRIAGE.md` Gate A3](TRIAGE.md) |
| 5 | **ANSI W-API 迁移**（中文游戏路径静默跳过） | 2 h | ⭐⭐⭐ | 🟡 | `crates/game-process/src/lib.rs:42-61` + `src/record/recorder.rs:425-432`，[`TRIAGE.md` Gate A5](TRIAGE.md) |
| 6 | **5-min auto-cap 定时器**（buyer spec 要求每片 5–6 分钟） | 2 h | ⭐⭐ | 🟢 | [`docs/RECORDER_BUYER_SPEC_FEATURES.md`](docs/RECORDER_BUYER_SPEC_FEATURES.md) |
| 7 | **F1/F2/F3 `route_type` 标签热键**（buyer schema 必填字段） | 4 h | ⭐⭐⭐ | 🟢 | [`docs/RECORDER_BUYER_SPEC_FEATURES.md`](docs/RECORDER_BUYER_SPEC_FEATURES.md) |
| 8 | **UI 元素拒收检测**（弹窗 / 菜单 / 桌面入镜 → 拒收 clip） | 1 d | ⭐⭐⭐⭐ | 🟢 | [`docs/RECORDER_BUYER_SPEC_FEATURES.md`](docs/RECORDER_BUYER_SPEC_FEATURES.md) |
| 9 | **Cyberpunk 2077 引擎 hook**（mock body → 真 RED4ext RTTI walk）| 2.5 d | ⭐⭐⭐⭐⭐ | 🔴 | [`crates/engine-telemetry/docs/CYBERPUNK_HOOK_RUNBOOK.md`](crates/engine-telemetry/docs/CYBERPUNK_HOOK_RUNBOOK.md) |
| 10 | **GTA V Enhanced 引擎 hook**（ScriptHookV `.asi` plugin + Rust FFI）| 3 d | ⭐⭐⭐⭐⭐ | 🔴 | [`crates/engine-telemetry/docs/GTA_V_HOOK_RUNBOOK.md`](crates/engine-telemetry/docs/GTA_V_HOOK_RUNBOOK.md) |

**价值图例**：🟢 业务关键（buyer 验收必需 / 用户体验）|🟡 稳定性（防止 v2.5.x 用户故障）|🔴 商业模型（2-4× 溢价的来源）

---

## 推荐路径

- **想 30 分钟跑通流程感觉一下**：挑 #1（mpsc 一行改动 + 加单元测试）
- **想做有结构的工程**：挑 #6 或 #7（buyer spec 已经 PR-ready，照着实施即可）
- **想做有挑战的前沿**：挑 #9 或 #10（scaffold + 22 个 Mac 测试 + 10-14 KB runbook 已就绪，等真人填 RTTI walk）

每一条都不是 toy task——任何一条做完合并都直接推动 v2.5.5 / v2.6 / v3.0 出版。

---

## 给救兵的话（meeting 中可直接念）

> "这 10 条都是我们项目当前真正卡住的工作，不是给你练手的脚手架。第 1 条 0.2 小时是入门 PR，让你跑通仓库的开发流程（cargo test + clippy + PR review）；第 9、10 条 2-3 天是项目最有意思的部分——Cyberpunk 和 GTA V 的引擎 hook，scaffold 都已经搭好了，等真人去填真实的 RTTI walk 和 ScriptHookV plugin。我们一直在找愿意做这两个的人。
>
> 你可以挑任意一条。挑哪条都比从零写一个新版本更接近"做出项目真正缺的东西"。"

---

## 时间承诺（你回应救兵预期管理用）

- 你**今天就能开始**：仓库已是 public，加 collaborator 一分钟搞定
- 你提 PR 的**响应承诺**：48 小时内 code review
- 你**入门期答疑**：每周 1 次 30 分钟 1:1（前 4 周）
- 你**质量门槛**：CI 全绿 + 1 reviewer approve（不需要等 Howard，puffydev 也能审）
