# Cloudflare Tunnel 配置指南

## 概述
使用 Cloudflare Tunnel 为 GameData Recorder 后端提供 HTTPS 访问，无需开放端口或配置防火墙。

## 前置要求
- Cloudflare 账户（免费版即可）
- 一个域名（已添加到 Cloudflare）

## 配置步骤

### 1. 登录 Cloudflare
```bash
ssh mac2-cdp
cloudflared tunnel login
```
- 会显示一个 URL，在浏览器中打开并授权
- 会生成 `~/.cloudflared/cert.pem` 证书文件

### 2. 创建 Tunnel
```bash
cloudflared tunnel create gamedata-backend
```
- 会输出 Tunnel ID（类似 `abc123def456`）
- 会生成 `~/.cloudflared/abc123def456.json` 凭证文件

### 3. 配置 Tunnel
创建配置文件 `~/.cloudflared/config.yml`：

```yaml
tunnel: <你的-tunnel-id>
credentials-file: /Users/<username>/.cloudflared/<你的-tunnel-id>.json

ingress:
  - hostname: api.gamedatalabs.com
    service: http://localhost:8080
  - service: http_status:404
```

### 4. 添加 DNS 记录
```bash
cloudflared tunnel route dns gamedata-backend api.gamedatalabs.com
```

### 5. 启动 Tunnel
```bash
cloudflared tunnel run gamedata-backend
```

### 6. 配置自动启动（launchd）
创建 `~/Library/LaunchAgents/com.cloudflare.tunnel.plist`：

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.cloudflare.tunnel</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/local/bin/cloudflared</string>
        <string>tunnel</string>
        <string>run</string>
        <string>gamedata-backend</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>/Users/<username>/.cloudflared/tunnel.log</string>
    <key>StandardErrorPath</key>
    <string>/Users/<username>/.cloudflared/tunnel.error.log</string>
</dict>
</plist>
```

加载配置：
```bash
launchctl load ~/Library/LaunchAgents/com.cloudflare.tunnel.plist
```

## 验证
```bash
# 测试 HTTPS 访问
curl https://api.gamedatalabs.com/health
# 应返回 {"status":"ok","version":"0.1.0"}
```

## 更新客户端配置
将 Windows 客户端的 API endpoint 改为：
```
https://api.gamedatalabs.com
```

## 故障排除

### 查看日志
```bash
tail -f ~/.cloudflared/tunnel.log
tail -f ~/.cloudflared/tunnel.error.log
```

### 重启 Tunnel
```bash
launchctl unload ~/Library/LaunchAgents/com.cloudflare.tunnel.plist
launchctl load ~/Library/LaunchAgents/com.cloudflare.tunnel.plist
```

### 检查 Tunnel 状态
```bash
cloudflared tunnel list
```
