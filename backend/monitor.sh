#!/bin/bash
# GameData Recorder - 后端监控脚本
# 部署在 Mac-2 上，定期检查服务健康状态

set -e

# 配置
API_URL="http://localhost:8080"
LOG_FILE="$HOME/gamedata-backend/monitor.log"
ALERT_WEBHOOK="${ALERT_WEBHOOK:-}"  # 可选：配置 Slack/Discord webhook
DATA_DIR="$HOME/gamedata-backend/data"

# 日志函数
log() {
    echo "[$(date '+%Y-%m-%d %H:%M:%S')] $1" | tee -a "$LOG_FILE"
}

# 健康检查
check_health() {
    local response
    response=$(curl -s -o /dev/null -w "%{http_code}" "$API_URL/health" 2>/dev/null || echo "000")
    
    if [ "$response" = "200" ]; then
        log "✅ Health check passed (HTTP 200)"
        return 0
    else
        log "❌ Health check failed (HTTP $response)"
        return 1
    fi
}

# 检查磁盘空间
check_disk_space() {
    local usage
    usage=$(df -h "$DATA_DIR" 2>/dev/null | awk 'NR==2 {print $5}' | sed 's/%//')
    
    if [ -z "$usage" ]; then
        usage=$(df -h "$HOME" | awk 'NR==2 {print $5}' | sed 's/%//')
    fi
    
    if [ "$usage" -gt 90 ]; then
        log "⚠️ WARNING: Disk usage is ${usage}% (>90%)"
        return 1
    elif [ "$usage" -gt 80 ]; then
        log "⚠️ Disk usage is ${usage}% (>80%)"
    else
        log "✅ Disk usage is ${usage}%"
    fi
    return 0
}

# 检查服务进程
check_process() {
    if pgrep -f "uvicorn main:app" > /dev/null; then
        log "✅ Backend process is running"
        return 0
    else
        log "❌ Backend process is NOT running"
        return 1
    fi
}

# 检查日志文件大小
check_log_size() {
    local log_file="$HOME/gamedata-backend/server.log"
    if [ -f "$log_file" ]; then
        local size_mb
        # Cross-platform stat: macOS uses -f%z, Linux uses -c%s
        if [[ "$OSTYPE" == "darwin"* ]]; then
            size_mb=$(stat -f%z "$log_file" 2>/dev/null | awk '{print $1/1024/1024}' || echo "0")
        else
            size_mb=$(stat -c%s "$log_file" 2>/dev/null | awk '{print $1/1024/1024}' || echo "0")
        fi
        if (( $(echo "$size_mb > 100" | bc -l) )); then
            log "⚠️ Log file size is ${size_mb}MB (>100MB), consider rotation"
        fi
    fi
}

# 重启服务
restart_service() {
    log "🔄 Restarting backend service..."
    launchctl unload ~/Library/LaunchAgents/com.gamedata.backend.plist 2>/dev/null || true
    sleep 2
    launchctl load ~/Library/LaunchAgents/com.gamedata.backend.plist
    sleep 3
    
    if check_health && check_process; then
        log "✅ Service restarted successfully"
        return 0
    else
        log "❌ Service restart failed"
        return 1
    fi
}

# 发送告警
send_alert() {
    local message="$1"
    
    if [ -n "$ALERT_WEBHOOK" ]; then
        # Slack/Discord webhook 支持
        curl -s -X POST -H 'Content-type: application/json' \
            --data "{\"text\":\"$message\"}" \
            "$ALERT_WEBHOOK" > /dev/null 2>&1 || true
    fi
    
    # 同时记录到系统日志
    logger -t gamedata-monitor "$message"
}

# 主函数
main() {
    log "=== Starting health check ==="
    
    local failed=0
    
    # 执行检查
    check_process || ((failed++))
    check_health || ((failed++))
    check_disk_space || ((failed++))
    check_log_size
    
    # 如果有检查失败，尝试重启
    if [ $failed -gt 0 ]; then
        log "⚠️ $failed check(s) failed, attempting restart..."
        send_alert "GameData Recorder backend health check failed on Mac-2"
        
        if restart_service; then
            send_alert "✅ GameData Recorder backend restarted successfully"
        else
            send_alert "❌ GameData Recorder backend restart failed - manual intervention required"
            exit 1
        fi
    fi
    
    log "=== Health check completed ==="
}

# 运行主函数
main "$@"
