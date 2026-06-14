---
task_id: S03-ci-obs-build-token
project: gamedata-recorder
priority: 2
estimated_minutes: 15
modifies: [".github/workflows/*.yml"]
executor: opencode
---

## 目标
消除 build-windows 的限流 flake：cargo-obs-build 步骤匿名调 GitHub API 拉 libobs release，runner 共享 IP 经常 403（实例: run 27441023892 首跑失败，rerun 过）。

## 实现
- 在所有运行 `cargo-obs-build`（含 `cargo obs-build`）的 workflow step 上注入环境变量:
  `GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}`（step 级 env，不要 job 级全局加）
- 先 grep .github/workflows/ 找到所有相关 step，逐个加；已有 GITHUB_TOKEN 的 step 跳过
- 在当前分支 fix/ci-obs-build-token 上提交（git checkout -b fix/ci-obs-build-token origin/main 先行），**不要 push**

## 验收
- [ ] 所有 cargo-obs-build step 都有 GITHUB_TOKEN env（grep 验证）
- [ ] yaml 语法有效（python3 -c "import yaml,glob; [yaml.safe_load(open(f)) for f in glob.glob('.github/workflows/*.yml')]" 通过）
- [ ] 除 workflow yml 外零文件改动

## 不要做
- 不动任何非 workflow 文件、不 push、不要询问
