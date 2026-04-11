# GameData Recorder - Windows 客户端测试清单

## 测试环境准备

### 1. 编译环境检查
- [ ] Windows 10/11 64位系统
- [ ] Rust 工具链已安装 (`rustc --version`)
- [ ] OBS Studio 已安装（用于录制功能）
- [ ] Visual Studio 2022 Build Tools 或完整版
- [ ] Git for Windows

### 2. 后端连接信息
```
API Endpoint: http://100.91.32.29:8080
Health Check: http://100.91.32.29:8080/health
API Docs: http://100.91.32.29:8080/docs
```

---

## 编译测试

### 3. 克隆和编译
```powershell
# 克隆代码
git clone https://github.com/howardleegeek/gamedata-recorder.git
cd gamedata-recorder

# 安装 OBS 构建工具
cargo install cargo-obs-build

# 安装 OBS 二进制文件
cargo obs-build build --out-dir target\x86_64-pc-windows-msvc\release

# 编译发布版本
cargo build --release
```

- [ ] 编译成功无错误
- [ ] 生成 `target\release\gamedata-recorder.exe`
- [ ] 所有 DLL 文件已复制到输出目录

---

## 功能测试

### 4. 启动测试
- [ ] 双击运行 `gamedata-recorder.exe`
- [ ] 系统托盘出现图标
- [ ] 无崩溃或错误弹窗
- [ ] 日志文件正常生成 (`%APPDATA%\GameData Recorder\logs\`)

### 5. 配置测试
- [ ] 右键托盘图标打开配置
- [ ] 修改录制路径为有效目录
- [ ] 设置 API endpoint 为 `http://100.91.32.29:8080`
- [ ] 保存配置后重新启动，配置持久化

### 6. 游戏检测测试
- [ ] 启动支持的游戏（Valorant/CS2/原神等）
- [ ] 客户端自动检测到游戏进程
- [ ] 托盘图标状态变为"就绪"

### 7. 录制功能测试
- [ ] 点击"开始录制"或快捷键
- [ ] 录制指示灯亮起
- [ ] 游戏画面正常录制（无卡顿）
- [ ] FPS 日志正常写入
- [ ] 点击"停止录制"
- [ ] 录制文件生成在指定目录
- [ ] 文件格式为 `.mp4` (H.265)

### 8. 数据上传测试
- [ ] 录制完成后自动上传元数据
- [ ] 检查后端 API 收到数据
- [ ] 收益计算正确显示

### 9. 长时间运行测试
- [ ] 连续录制 30 分钟
- [ ] 内存占用稳定（无内存泄漏）
- [ ] 磁盘空间正常释放（临时文件清理）
- [ ] 系统不卡顿

---

## 兼容性测试

### 10. 多游戏测试
- [ ] Valorant
- [ ] Counter-Strike 2
- [ ] 原神
- [ ] 英雄联盟
- [ ] Apex Legends

### 11. 系统兼容性
- [ ] Windows 10 21H2+
- [ ] Windows 11 22H2+
- [ ] NVIDIA GPU (H.265 硬件编码)
- [ ] AMD GPU (H.265 硬件编码)
- [ ] Intel GPU (QSV 编码)

---

## 异常处理测试

### 12. 错误场景
- [ ] 磁盘空间不足时的提示
- [ ] 网络断开时的重试机制
- [ ] 游戏崩溃时录制正常停止
- [ ] 权限不足时的友好提示
- [ ] 重复启动时的单实例检查

---

## 性能测试

### 13. 性能指标
- [ ] CPU 占用 < 10% (i5-12400F 级别)
- [ ] 内存占用 < 500MB
- [ ] 录制时游戏帧率下降 < 5%
- [ ] 1080p60 录制文件大小合理 (~100MB/分钟)

---

## 验收标准

所有测试项通过后才能进入用户测试阶段：
- ✅ 编译成功
- ✅ 基础功能正常
- ✅ 至少 3 款游戏测试通过
- ✅ 长时间运行稳定
- ✅ 异常处理完善

---

## 测试记录模板

```markdown
## 测试日期: YYYY-MM-DD

### 测试环境
- OS: Windows 11 22H2
- CPU: Intel i7-12700K
- GPU: NVIDIA RTX 3070
- RAM: 32GB

### 测试结果
- 编译: ✅/❌
- 启动: ✅/❌
- 录制: ✅/❌
- 上传: ✅/❌

### 发现问题
1. ...
2. ...

### 截图/日志
[附件]
```
