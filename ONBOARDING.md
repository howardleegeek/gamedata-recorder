# GameData Recorder — Engineer Onboarding

*Last updated: 2026-05-11 · Owner: Howard Li (CEO, Oysterworld Inc.) · Audience: incoming engineer*

---

## 1. Executive Summary

**GameData Recorder** 是 GameData Labs 旗下的 Windows 桌面客户端，自动捕获玩家的游戏画面、输入流与引擎遥测数据，经清洗后出售给训练世界模型（World Model）与具身智能（Embodied AI）的下游买方。

项目派生自 Overworld AI 的开源项目 **OWL Control**（MIT 协议），Howard 的分叉在其基础上完成了生产化改造、buyer-spec schema 兼容层、以及引擎遥测 hook 框架。

代码库规模约 15 k LOC Rust（客户端）+ Python FastAPI（后端），当前已发布 v2.6.0，处于面向小规模友好测试用户的灰度阶段。

**新工程师定位**：Rust / Windows 系统级开发，承接以下任一方向——客户端稳定性修复、引擎遥测 hook 实现（Cyberpunk 2077 / GTA V）、或 buyer-spec 功能交付。

---

## 2. Business Context

### 2.1 产品定位

| 维度 | 内容 |
|---|---|
| 一句话价值主张 | "Play games. Record screen. Get paid." |
| 终端用户 | PC 游戏玩家（Windows 10/11，独立 GPU） |
| 付费方 | AI 公司（购买训练数据） |
| 单用户月收入 | ~$10–$40，含引擎元数据可达 2–4× |
| 当前阶段 | 灰度（v2.6.0），目标百量级 tester |

### 2.2 数据价值梯度

| 数据层 | 价值乘数 | 实现状态 |
|---|---|---|
| 屏幕视频（H.265，1080p30） | 1× | 已实现 |
| 输入流（键鼠 + 手柄，JSONL） | 1.5× | 已实现 |
| 引擎元数据（坐标、旋转、FOV、GPU） | 2–4× | 部分实现 |
| 深度缓冲（Cyberpunk 2077） | 3–5× | Scaffold 完成 |

### 2.3 合规边界（重要）

- 仅在游戏前台时捕获；alt-tab 或退出立即停止
- 不录制桌面、浏览器、非游戏程序
- 不使用任何会触发反作弊系统（EAC / BattlEye / VAC）的注入或 hook
- 服务端对 PII（人脸、通知中的用户名）做模糊处理
- 全部上传走 TLS 1.3
- 客户端 MIT 开源，用户可审计任意一行

任何改动若可能突破上述边界，**必须在 PR 中显式标注并升级至 Howard 审批**。

---

## 3. Technical Stack

| 层 | 技术 |
|---|---|
| 客户端语言 | Rust（stable，1.75+） |
| 捕获栈 | OBS Studio SDK（embedded）+ Windows DXGI Desktop Duplication |
| 视频编码 | NVENC / AMF / Intel QSV（运行时探测） |
| 输入捕获 | Win32 Raw Input API（W-suffix wide APIs） |
| UI | Tray icon + 非激活 overlay（`SW_SHOWNA`） |
| 引擎 hook | RED4ext（Cyberpunk）/ ScriptHookV `.asi`（GTA V） |
| 异步运行时 | tokio（multi-thread runtime） |
| IPC / 通道 | `tokio::sync::mpsc`（**bounded only**） |
| 后端 | Python 3.11 + FastAPI + Alembic + PostgreSQL |
| 部署 | Cloudflare Tunnel + Docker（参见 `docs/CLOUDFLARE_TUNNEL_SETUP.md`） |
| CI | GitHub Actions（`.github/workflows/build.yml`） |

---

## 4. Repository & Access Provisioning

### 4.1 仓库

| 资源 | 地址 |
|---|---|
| 主仓库（public） | https://github.com/howardleegeek/gamedata-recorder |
| 官网 | https://gamedata-recorder.vercel.app |
| Windows 测试机 | NUC Box（WSL remote 别名 `nucbox`） |

### 4.2 接入清单

新工程师入职第一天需完成：

- [ ] GitHub 账号已被加为 collaborator（push 权限）
- [ ] 本地 clone 并通过 `cargo check` 编译
- [ ] 加入沟通渠道（待 Howard 拉群）
- [ ] 阅读完成 §13 的必读文档
- [ ] 获取 NUC Box SSH 凭证（用于 Windows 集成测试）

### 4.3 本地环境要求

- Rust toolchain（`rustup default stable`）
- Windows 10/11 运行环境（runtime 部分）或 macOS（仅可跑 crate 单元测试）
- Python 3.11+（后端开发）
- `gh` CLI（PR 流程）

---

## 5. Codebase Structure

```
gamedata-recorder/
├── src/                          # 主程序（Windows-only runtime）
│   ├── record/                   # 捕获核心：OBS recorder、input/FPS logger
│   ├── ui/                       # tray、overlay、notification（禁用 modal）
│   ├── tokio_thread.rs           # 主事件循环
│   ├── config.rs                 # 持久化配置、热键迁移
│   └── validation/               # 录制完整性校验（JSONL/CSV 解析）
├── crates/                       # 6 个独立 crate（macOS CI 可跑）
│   ├── constants/                # 白名单 / 黑名单 / 容量参数
│   ├── game-process/             # 进程扫描 + 游戏检测（W-API）
│   ├── input-capture/            # 键鼠 / 手柄输入流
│   ├── engine-telemetry/         # Cyberpunk + GTA V hook 框架
│   ├── depth-hook/               # Cyberpunk 深度缓冲（实验性）
│   └── action-camera-tests/      # 30 个跨平台测试
├── backend/                      # FastAPI + Alembic
├── docs/                         # 22 份技术文档（详见 §13）
├── installer/                    # NSIS Windows 安装包
├── build-resources/              # 图标、签名资产
└── scripts/                      # 构建、测试、CI 辅助脚本
```

---

## 6. Release Cadence & Current Version

- 当前 latest：**v2.6.0**（2026-04-22）
- 发布节奏：bug fix patch 按需，feature minor 每周一次
- 发布物：GitHub Release 自动产出 `gamedata-recorder-<version>-windows-x86_64.zip`
- 版本号语义：`MAJOR.MINOR.PATCH`，灰度内 PATCH 自动 OTA，MINOR 需用户确认

---

## 7. Roadmap — Three Quality Gates

详见 [TRIAGE.md](TRIAGE.md)。当前共 40 条 audit findings（5 轮并行 audit 产出），分三道门：

### Gate A — v2.5.5（当周）

目标：当前付费客户能在中文 Win11 + RTX 4060 上端到端跑通 GTA V 完整录制。
范围：5 个 critical 修复，预计 6 小时工程量。
门控人：Howard。

### Gate B — v2.6（本月）

目标：支撑 100 名友好 tester 稳定运行。
范围：约 12 项稳定性与正确性修复，含真实 GPU/FPS/FOV 元数据、非 ASCII locale、Workstation lock 恢复。

### Gate C — v3.0（季度内）

目标：公开 payout 通道前的安全 / 反欺诈 / 隐私基线。
范围：约 18 项，含 content attestation、binary signature 校验、HID-replay 检测、DPAPI key protection、Authenticode 签名。

---

## 8. Active Workstreams & Ownership

| 工作流 | 入口文档 | 状态 | 当前 owner |
|---|---|---|---|
| Gate A critical fixes | `TRIAGE.md` §Gate A | 进行中 | 待分配 |
| Buyer-spec features（F1/F2/F3 路由标签、5 分钟自动切片、UI 拒收） | `docs/RECORDER_BUYER_SPEC_FEATURES.md` | Spec PR-ready | puffydev |
| Cyberpunk 2077 hook fill | `crates/engine-telemetry/docs/CYBERPUNK_HOOK_RUNBOOK.md` | Scaffold 完成 | 待分配 |
| GTA V Enhanced hook fill | `crates/engine-telemetry/docs/GTA_V_HOOK_RUNBOOK.md` | Scaffold 完成 | 待分配 |
| Backend MVP（upload / dashboard / payout） | `backend/CLAUDE.md` | 8 commits | Howard |

新工程师可与 Howard 协商承接其中一条，建议从 **Gate A** 起步以快速建立代码熟悉度。

---

## 9. Engineering Standards

### 9.1 Commit 规范

Conventional Commits：

```
<type>(<scope>): <subject>

[optional body]
[optional footer]
```

`type` ∈ {`feat`, `fix`, `refactor`, `docs`, `test`, `chore`, `perf`, `ci`}。

### 9.2 Branch 策略

- `main` 永远可发布
- 功能分支：`feat/<short-desc>`
- 修复分支：`fix/<bug-id>-<short-desc>`
- 直接 push `main` 仅限 Howard 在紧急 hotfix 场景

### 9.3 PR 流程

1. 自检 `cargo fmt && cargo clippy -- -D warnings && cargo test`
2. 在 PR 描述中包含：变更目的、影响面、测试方式、回滚步骤
3. 至少 1 名 reviewer approve（Howard 或指定 reviewer）
4. 涉及合规边界（§2.3）或基础设施的变更需 Howard 显式 approve
5. 通过 CI 后由 reviewer 合并（squash merge 默认）

### 9.4 Definition of Done

一项工作视为完成需同时满足：

- [ ] 代码已合入 `main`
- [ ] 单元测试覆盖关键路径，CI 全绿
- [ ] 在 NUC Box 上至少完成一次端到端验证（涉及 runtime 行为时）
- [ ] 相关文档（README / BUGS / TRIAGE 等）已同步更新
- [ ] 若引入新错误码或日志键，已记录到 `docs/api-spec.md`

### 9.5 测试约束

- 任何对 `EngineFrame` 字段名或数组顺序的改动**必须**同步更新 buyer plugin 测试，且需 Howard 二次确认（schema 是与买方的契约，破坏即静默丢失训练样本）
- macOS 上可运行的 crate 测试：`cargo test -p engine-telemetry --target aarch64-apple-darwin`
- Windows 端到端测试见 `WINDOWS_TEST_CHECKLIST.md`

---

## 10. Known Pitfalls & Architectural Constraints

以下条目均来自真实事故，违反任意一条会引发已记录的客户故障，请在动手前确认理解：

1. **禁止任何 modal popup**（`MessageBox*`）在录制运行期出现——会抢走全屏游戏的焦点。统一使用 `tracing::warn!` / `tracing::error!`。仅启动前的致命错误可以弹一次。
2. **`EngineFrame` schema 是与下游 buyer plugin 的契约**——字段重命名或数组顺序变更会导致 zero training samples 且无错误信号。改 schema 必须协同 buyer。
3. **Windows API 一律使用 W-suffix wide 版本**（`PROCESSENTRY32W`、`QueryFullProcessImageNameW`）。ANSI 版本对中文路径会静默跳过。
4. **mpsc channel 必须 bounded**——`unbounded_channel()` 在 60+ events/s 输入下会无限增长，与游戏抢内存导致 OOM（已发生在 GTA V 16 GB 客户机）。
5. **默认热键不得占用游戏常用按键**——F5 是大多数游戏的 quick save，已迁移到 F9。
6. **Overlay 显示必须使用 `SW_SHOWNA`** 而非 `SW_SHOWDEFAULT`——后者会激活窗口并最小化游戏。
7. **约 80% 的代码异味继承自上游 OWL Control**——动手修改前先 `git blame`，确认是否是上游遗留 TODO，避免误判为退化。
8. **运行时仅支持 Windows**——crate 单元测试可跨平台，但完整集成测试必须在 Windows 上完成。
9. **不要触碰 anti-cheat 高危区域**——任何形式的进程注入、DLL hook、内存读写都需事先与 Howard 沟通。
10. **配置文件读取需向后兼容**——`config.json` 在历史版本间存在 schema 漂移，新字段必须带默认值。

完整事故复盘见 [BUGS.md](BUGS.md)（19 类 bug，每条含 root cause 与 fix 引用）。

---

## 11. Onboarding Plan

### Day 1（入职当天）

- 完成 §4.2 接入清单
- 阅读：`README.md` → `ONBOARDING.md`（本文档）→ `BUGS.md`
- 本地 `cargo check` 通过
- 在沟通群里完成自我介绍

### Week 1

- 阅读：`TRIAGE.md`、`docs/PUFFYDEV_BRIEF_2026_05_01.md`、`docs/ARCHITECTURE.md`、`docs/RECORDER_BUYER_SPEC_FEATURES.md`
- 完成首个 PR（建议从 Gate A 的 **A4: mpsc capacity 10→10_000** 入手，预计工程量 0.2 小时）
- 走通 NUC Box 上的端到端测试流程
- 与 Howard 完成一次 30 分钟同步，确定主线工作流归属

### Month 1

- 独立承接并交付一条 Gate A 或 Gate B 工作流
- 累计 ≥ 5 个 merged PR
- 对所选模块（`src/record/` / `crates/engine-telemetry/` / `backend/` 任一）形成 owner 级理解
- 在 review 阶段能识别上游 OWL 遗留 vs. 新引入退化

---

## 12. Communication & Escalation

### 12.1 日常沟通

- 工作时区：Howard 在美西（PT），新工程师按本地工作时间即可
- 异步优先：技术问题先在 PR 或 Issue 评论中提，避免同步打断
- 同步窗口：每周 1 次 30 分钟 1:1（时间另约）

### 12.2 升级路径

| 情境 | 处理方式 |
|---|---|
| 不确定 spec 的实现细节 | 在 Issue / PR 评论中 @Howard，附带方案 A/B 选项与建议 |
| 触及合规边界（§2.3） | 暂停实现，先在 PR 草稿中提出方案待审批 |
| 触及 buyer schema（`EngineFrame`） | 必须先获得 Howard 明确确认 |
| 发现可能影响线上灰度用户的退化 | 立即 @Howard，必要时回滚发布 |
| 工程量评估偏差 > 2× | 主动通知，重新对齐范围 |

### 12.3 报告

- 周报：每周五前在仓库 `docs/weekly/` 下提交一份 `<yyyy-mm-dd>-<name>.md`，含：本周完成、下周计划、阻塞项

---

## 13. Reading List

按阅读优先级排序：

| 优先级 | 文档 | 内容 |
|---|---|---|
| P0 | [README.md](README.md) | 产品定位、用户视角 |
| P0 | [ONBOARDING.md](ONBOARDING.md) | 本文档 |
| P0 | [BUGS.md](BUGS.md) | 19 类已知 bug + root cause |
| P0 | [TRIAGE.md](TRIAGE.md) | 40 条 audit findings + 三道质量门 |
| P1 | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | 系统架构与模块边界 |
| P1 | [docs/RECORDER_BUYER_SPEC_FEATURES.md](docs/RECORDER_BUYER_SPEC_FEATURES.md) | Buyer 侧 schema 与功能契约 |
| P1 | [docs/PUFFYDEV_BRIEF_2026_05_01.md](docs/PUFFYDEV_BRIEF_2026_05_01.md) | 上一份 Windows 工程师 brief（相邻工作流） |
| P1 | [BUILD.md](BUILD.md) | 本地构建步骤 |
| P1 | [WINDOWS_TEST_CHECKLIST.md](WINDOWS_TEST_CHECKLIST.md) | Windows 端到端验收清单 |
| P2 | [docs/MULTI_GAME_ROADMAP.md](docs/MULTI_GAME_ROADMAP.md) | 多游戏支持路线图 |
| P2 | [docs/CAPTURE_PERFORMANCE_INVESTIGATION.md](docs/CAPTURE_PERFORMANCE_INVESTIGATION.md) | 捕获性能调查报告 |
| P2 | [docs/api-spec.md](docs/api-spec.md) | Backend API 规范 |
| P2 | [docs/DEPLOYMENT.md](docs/DEPLOYMENT.md) | 后端部署 |
| P3 | [docs/COMPREHENSIVE_REVIEW_REPORT.md](docs/COMPREHENSIVE_REVIEW_REPORT.md) | 综合审计报告 |
| P3 | [docs/NUCBOX_SETUP_SOP.md](docs/NUCBOX_SETUP_SOP.md) | NUC Box Windows 测试机 SOP |
| P3 | [CONTRIBUTING.md](CONTRIBUTING.md) | 上游贡献者指南（OWL 继承） |

---

*文档结束。如有遗漏或不清楚的部分，请直接在仓库提 Issue 并标记 `onboarding`。*
