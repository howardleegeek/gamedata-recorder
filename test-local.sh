#!/bin/bash
# 本地快速测试脚本 - 验证系统真的能跑起来
# 用法: bash test-local.sh

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

API_URL="${API_URL:-http://100.91.32.29:8080}"
FAILED=0

log_info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

# 测试 1: 健康检查
test_health() {
    log_info "Testing health endpoint..."
    RESPONSE=$(curl -s "${API_URL}/health" 2>/dev/null || echo "FAILED")
    
    if echo "$RESPONSE" | grep -q '"status":"ok"'; then
        log_info "✅ Health check passed"
        echo "Response: $RESPONSE" | jq . 2>/dev/null || echo "$RESPONSE"
        return 0
    else
        log_error "❌ Health check failed"
        echo "Response: $RESPONSE"
        return 1
    fi
}

# 测试 2: API 文档
test_docs() {
    log_info "Testing API docs..."
    STATUS=$(curl -s -o /dev/null -w "%{http_code}" "${API_URL}/docs" 2>/dev/null)
    
    if [ "$STATUS" = "200" ]; then
        log_info "✅ API docs accessible (HTTP 200)"
        return 0
    else
        log_error "❌ API docs failed (HTTP $STATUS)"
        return 1
    fi
}

# 测试 3: 登录流程
test_login() {
    log_info "Testing login endpoint..."
    
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/auth/login" \
        -H "Content-Type: application/json" \
        -d '{"email": "local_test@example.com", "provider": "email"}' 2>/dev/null || echo "FAILED")
    
    if echo "$RESPONSE" | grep -q '"token"'; then
        TOKEN=$(echo "$RESPONSE" | jq -r '.token' 2>/dev/null)
        log_info "✅ Login successful"
        log_info "Token: ${TOKEN:0:30}..."
        
        # 导出 token 供后续测试使用
        export TEST_TOKEN="$TOKEN"
        return 0
    else
        log_error "❌ Login failed"
        echo "Response: $RESPONSE"
        return 1
    fi
}

# 测试 4: 用户信息
test_user_info() {
    log_info "Testing user info endpoint..."
    
    if [ -z "$TEST_TOKEN" ]; then
        log_warn "No token available, skipping user info test"
        return 0
    fi
    
    RESPONSE=$(curl -s "${API_URL}/api/v1/user/info" \
        -H "Authorization: Bearer ${TEST_TOKEN}" 2>/dev/null || echo "FAILED")
    
    if echo "$RESPONSE" | grep -q '"user_id"'; then
        USER_ID=$(echo "$RESPONSE" | jq -r '.user_id' 2>/dev/null)
        log_info "✅ User info retrieved"
        log_info "User ID: $USER_ID"
        return 0
    else
        log_error "❌ User info failed"
        echo "Response: $RESPONSE"
        return 1
    fi
}

# 测试 5: 上传初始化
test_upload_init() {
    log_info "Testing upload initialization..."
    
    if [ -z "$TEST_TOKEN" ]; then
        log_warn "No token available, skipping upload test"
        return 0
    fi
    
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TEST_TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "test_video.mp4",
            "total_size_bytes": 104857600,
            "game_exe": "test_game.exe",
            "video_duration_seconds": 60,
            "video_width": 1920,
            "video_height": 1080,
            "video_codec": "h265",
            "video_fps": 60
        }' 2>/dev/null || echo "FAILED")
    
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        UPLOAD_ID=$(echo "$RESPONSE" | jq -r '.upload_id' 2>/dev/null)
        log_info "✅ Upload initialization successful"
        log_info "Upload ID: $UPLOAD_ID"
        return 0
    else
        log_error "❌ Upload initialization failed"
        echo "Response: $RESPONSE"
        return 1
    fi
}

# 测试 6: 收益查询
test_earnings() {
    log_info "Testing earnings endpoint..."
    
    if [ -z "$TEST_TOKEN" ]; then
        log_warn "No token available, skipping earnings test"
        return 0
    fi
    
    RESPONSE=$(curl -s "${API_URL}/api/v1/earnings/summary" \
        -H "Authorization: Bearer ${TEST_TOKEN}" 2>/dev/null || echo "FAILED")
    
    if echo "$RESPONSE" | grep -q '"total_usd"'; then
        TOTAL=$(echo "$RESPONSE" | jq -r '.total_usd' 2>/dev/null)
        log_info "✅ Earnings retrieved"
        log_info "Total earnings: $${TOTAL}"
        return 0
    else
        log_error "❌ Earnings query failed"
        echo "Response: $RESPONSE"
        return 1
    fi
}

# 测试 7: CORS 配置
test_cors() {
    log_info "Testing CORS configuration..."
    
    RESPONSE=$(curl -s -X OPTIONS \
        -H "Origin: http://localhost:3000" \
        -H "Access-Control-Request-Method: POST" \
        -i "${API_URL}/api/v1/login" 2>/dev/null | grep -i "access-control" || echo "")
    
    if [ -n "$RESPONSE" ]; then
        log_info "✅ CORS headers present"
        return 0
    else
        log_warn "⚠️ CORS headers not detected (might be OK)"
        return 0
    fi
}

# 主函数
main() {
    echo "=========================================="
    echo "  GameData Recorder - Local Test Suite"
    echo "=========================================="
    echo ""
    echo "API URL: $API_URL"
    echo "Time: $(date)"
    echo ""
    
    # 检查依赖
    if ! command -v curl &> /dev/null; then
        log_error "curl is required but not installed"
        exit 1
    fi
    
    # 运行所有测试
    test_health || ((FAILED++))
    test_docs || ((FAILED++))
    test_login || ((FAILED++))
    test_user_info || ((FAILED++))
    test_upload_init || ((FAILED++))
    test_earnings || ((FAILED++))
    test_cors || true  # CORS 测试失败不阻塞
    
    # 总结
    echo ""
    echo "=========================================="
    if [ $FAILED -eq 0 ]; then
        log_info "✅ All tests passed! System is working correctly."
        echo ""
        echo "Next steps:"
        echo "  1. Test Windows client compilation"
        echo "  2. Run Windows client and connect to $API_URL"
        echo "  3. Verify end-to-end recording and upload"
        exit 0
    else
        log_error "❌ $FAILED test(s) failed!"
        echo ""
        echo "Troubleshooting:"
        echo "  1. Check if backend is running: ssh mac2-cdp 'launchctl list | grep gamedata'"
        echo "  2. Check backend logs: ssh mac2-cdp 'tail -f ~/gamedata-backend/server.log'"
        echo "  3. Restart backend: ssh mac2-cdp 'launchctl unload ~/Library/LaunchAgents/com.gamedata.backend.plist && launchctl load ~/Library/LaunchAgents/com.gamedata.backend.plist'"
        exit 1
    fi
}

# 运行
main "$@"
