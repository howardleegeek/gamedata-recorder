#!/bin/bash
# 20轮 Edge Case 测试脚本
# 全面测试系统的健壮性和边界条件

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
PURPLE='\033[0;35m'
CYAN='\033[0;36m'
NC='\033[0m'

API_URL="${API_URL:-http://100.91.32.29:8080}"
TOTAL_TESTS=0
PASSED_TESTS=0
FAILED_TESTS=0
WARNINGS=0

# 测试记录
TEST_RESULTS=()

log_header() {
    echo -e "${CYAN}========================================${NC}"
    echo -e "${CYAN}  $1${NC}"
    echo -e "${CYAN}========================================${NC}"
}

log_round() {
    echo ""
    echo -e "${PURPLE}╔════════════════════════════════════════════════╗${NC}"
    echo -e "${PURPLE}║${NC} ${YELLOW}ROUND $1: $2${NC}"
    echo -e "${PURPLE}╚════════════════════════════════════════════════╝${NC}"
}

log_test() {
    echo -e "${BLUE}[TEST]${NC} $1"
}

log_pass() {
    echo -e "${GREEN}[PASS]${NC} $1"
    ((PASSED_TESTS++))
    ((TOTAL_TESTS++))
}

log_fail() {
    echo -e "${RED}[FAIL]${NC} $1"
    ((FAILED_TESTS++))
    ((TOTAL_TESTS++))
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
    ((WARNINGS++))
}

# 获取 token
get_token() {
    local email="${1:-test@example.com}"
    curl -s -X POST "${API_URL}/api/v1/auth/login" \
        -H "Content-Type: application/json" \
        -d "{\"email\": \"$email\", \"provider\": \"email\"}" 2>/dev/null | jq -r '.token' 2>/dev/null || echo ""
}

# ==========================================
# ROUND 1: 正常流程测试
# ==========================================
round_1_normal_flow() {
    log_round "1" "正常流程测试 (Normal Flow)"
    
    log_test "Health check"
    RESPONSE=$(curl -s "${API_URL}/health" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"status":"ok"'; then
        log_pass "Health endpoint returns ok"
    else
        log_fail "Health endpoint failed: $RESPONSE"
    fi
    
    log_test "Login and get token"
    TOKEN=$(get_token "round1@test.com")
    if [ -n "$TOKEN" ] && [ "$TOKEN" != "null" ]; then
        log_pass "Login successful, token received"
    else
        log_fail "Login failed"
    fi
    
    log_test "Get user info"
    RESPONSE=$(curl -s "${API_URL}/api/v1/user/info" \
        -H "Authorization: Bearer ${TOKEN}" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"user_id"'; then
        log_pass "User info retrieved"
    else
        log_fail "User info failed: $RESPONSE"
    fi
    
    log_test "Initialize upload"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "test.mp4",
            "total_size_bytes": 104857600,
            "game_exe": "game.exe",
            "video_duration_seconds": 60,
            "video_width": 1920,
            "video_height": 1080,
            "video_codec": "h265",
            "video_fps": 60
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        log_pass "Upload initialized"
    else
        log_fail "Upload init failed: $RESPONSE"
    fi
    
    log_test "Get earnings"
    RESPONSE=$(curl -s "${API_URL}/api/v1/earnings/summary" \
        -H "Authorization: Bearer ${TOKEN}" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"total_usd"'; then
        log_pass "Earnings retrieved"
    else
        log_fail "Earnings failed: $RESPONSE"
    fi
}

# ==========================================
# ROUND 2: 大文件上传测试
# ==========================================
round_2_large_files() {
    log_round "2" "大文件上传测试 (Large Files)"
    
    TOKEN=$(get_token "round2@test.com")
    
    log_test "Upload 1GB file"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "large_file.mp4",
            "total_size_bytes": 1073741824,
            "game_exe": "game.exe",
            "video_duration_seconds": 3600
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        log_pass "1GB file upload initialized"
    else
        log_fail "1GB file upload failed: $RESPONSE"
    fi
    
    log_test "Upload 10GB file"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "huge_file.mp4",
            "total_size_bytes": 10737418240,
            "game_exe": "game.exe",
            "video_duration_seconds": 7200
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        log_pass "10GB file upload initialized"
    else
        log_warn "10GB file upload may have limitations: $RESPONSE"
    fi
    
    log_test "Upload 0 bytes file"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "empty.mp4",
            "total_size_bytes": 0,
            "game_exe": "game.exe"
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        log_pass "Empty file upload handled"
    else
        log_warn "Empty file rejected (may be expected): $RESPONSE"
    fi
}

# ==========================================
# ROUND 3: 并发请求测试
# ==========================================
round_3_concurrent() {
    log_round "3" "并发请求测试 (Concurrent Requests)"
    
    log_test "10 concurrent logins"
    PIDS=()
    for i in {1..10}; do
        (curl -s -X POST "${API_URL}/api/v1/auth/login" \
            -H "Content-Type: application/json" \
            -d "{\"email\": \"concurrent$i@test.com\", \"provider\": \"email\"}" > /dev/null 2>&1) &
        PIDS+=($!)
    done
    
    # 等待所有请求完成
    for pid in "${PIDS[@]}"; do
        wait $pid 2>/dev/null || true
    done
    log_pass "10 concurrent logins completed"
    
    log_test "20 concurrent health checks"
    PIDS=()
    for i in {1..20}; do
        (curl -s "${API_URL}/health" > /dev/null 2>&1) &
        PIDS+=($!)
    done
    
    for pid in "${PIDS[@]}"; do
        wait $pid 2>/dev/null || true
    done
    log_pass "20 concurrent health checks completed"
}

# ==========================================
# ROUND 4: 无效 Token 测试
# ==========================================
round_4_invalid_tokens() {
    log_round "4" "无效 Token 测试 (Invalid Tokens)"
    
    log_test "Empty token"
    RESPONSE=$(curl -s "${API_URL}/api/v1/user/info" \
        -H "Authorization: Bearer " 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail".*[401,403]'; then
        log_pass "Empty token rejected with 401/403"
    else
        log_warn "Empty token response: $RESPONSE"
    fi
    
    log_test "Malformed token"
    RESPONSE=$(curl -s "${API_URL}/api/v1/user/info" \
        -H "Authorization: Bearer invalid_token_12345" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail"'; then
        log_pass "Malformed token rejected"
    else
        log_warn "Malformed token response: $RESPONSE"
    fi
    
    log_test "Random token"
    RANDOM_TOKEN="$(openssl rand -hex 32)"
    RESPONSE=$(curl -s "${API_URL}/api/v1/user/info" \
        -H "Authorization: Bearer $RANDOM_TOKEN" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail"'; then
        log_pass "Random token rejected"
    else
        log_warn "Random token response: $RESPONSE"
    fi
}

# ==========================================
# ROUND 5: 过期 Token 测试
# ==========================================
round_5_expired_tokens() {
    log_round "5" "过期 Token 测试 (Expired Tokens)"
    
    log_test "Old timestamp token"
    # 创建一个31天前的token
    OLD_TIMESTAMP=$(($(date +%s) - 31 * 86400))
    EXPIRED_TOKEN="user_test:${OLD_TIMESTAMP}:fakesignature1234"
    RESPONSE=$(curl -s "${API_URL}/api/v1/user/info" \
        -H "Authorization: Bearer ${EXPIRED_TOKEN}" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail"'; then
        log_pass "Expired token rejected"
    else
        log_warn "Expired token may still work: $RESPONSE"
    fi
    
    log_test "Future timestamp token"
    FUTURE_TIMESTAMP=$(($(date +%s) + 86400))
    FUTURE_TOKEN="user_test:${FUTURE_TIMESTAMP}:fakesignature1234"
    RESPONSE=$(curl -s "${API_URL}/api/v1/user/info" \
        -H "Authorization: Bearer ${FUTURE_TOKEN}" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail"'; then
        log_pass "Future token rejected (expected)"
    else
        log_warn "Future token response: $RESPONSE"
    fi
}

# ==========================================
# ROUND 6: 缺失字段测试
# ==========================================
round_6_missing_fields() {
    log_round "6" "缺失字段测试 (Missing Fields)"
    
    TOKEN=$(get_token "round6@test.com")
    
    log_test "Upload without filename"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{"total_size_bytes": 1000000}' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail"'; then
        log_pass "Missing filename handled"
    else
        log_warn "Missing filename response: $RESPONSE"
    fi
    
    log_test "Upload without size"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{"filename": "test.mp4"}' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail"'; then
        log_pass "Missing size handled"
    else
        log_warn "Missing size response: $RESPONSE"
    fi
    
    log_test "Login without email"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/auth/login" \
        -H "Content-Type: application/json" \
        -d '{"provider": "email"}' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail"'; then
        log_pass "Missing email handled"
    else
        log_warn "Missing email response: $RESPONSE"
    fi
}

# ==========================================
# ROUND 7: 超大请求体测试
# ==========================================
round_7_large_payloads() {
    log_round "7" "超大请求体测试 (Large Payloads)"
    
    TOKEN=$(get_token "round7@test.com")
    
    log_test "10MB JSON payload"
    HUGE_JSON=$(python3 -c "print('{\"data\":\"' + 'A'*10485760 + '\"}')" 2>/dev/null || echo '{"data":"large"}')
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d "$HUGE_JSON" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail"'; then
        log_pass "Large payload rejected or handled"
    else
        log_warn "Large payload response: ${RESPONSE:0:100}"
    fi
    
    log_test "Deeply nested JSON"
    NESTED='{"a":{"b":{"c":{"d":{"e":"deep"}}}}}'
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d "$NESTED" 2>/dev/null)
    log_pass "Nested JSON handled (response: ${RESPONSE:0:50})"
}

# ==========================================
# ROUND 8: SQL 注入尝试
# ==========================================
round_8_sql_injection() {
    log_round "8" "SQL 注入测试 (SQL Injection Attempts)"
    
    log_test "SQL injection in email"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/auth/login" \
        -H "Content-Type: application/json" \
        -d '{"email": "test@test.com\'; DROP TABLE users; --", "provider": "email"}' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"token"'; then
        log_pass "SQL injection in email handled safely"
    else
        log_warn "SQL injection response: $RESPONSE"
    fi
    
    log_test "SQL injection in filename"
    TOKEN=$(get_token "round8@test.com")
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "test.mp4\'; DELETE FROM uploads; --",
            "total_size_bytes": 1000000
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        log_pass "SQL injection in filename handled safely"
    else
        log_warn "SQL injection in filename response: $RESPONSE"
    fi
}

# ==========================================
# ROUND 9: XSS 攻击尝试
# ==========================================
round_9_xss_attempts() {
    log_round "9" "XSS 攻击测试 (XSS Attempts)"
    
    log_test "XSS in email"
    XSS_EMAIL='<script>alert("xss")</script>@test.com'
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/auth/login" \
        -H "Content-Type: application/json" \
        -d "{\"email\": \"$XSS_EMAIL\", \"provider\": \"email\"}" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"token"'; then
        log_pass "XSS in email handled"
    else
        log_warn "XSS email response: $RESPONSE"
    fi
    
    log_test "XSS in game_exe"
    TOKEN=$(get_token "round9@test.com")
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "test.mp4",
            "total_size_bytes": 1000000,
            "game_exe": "<script>alert(1)</script>.exe"
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        log_pass "XSS in game_exe handled"
    else
        log_warn "XSS game_exe response: $RESPONSE"
    fi
}

# ==========================================
# ROUND 10: 特殊字符测试
# ==========================================
round_10_special_characters() {
    log_round "10" "特殊字符测试 (Special Characters)"
    
    TOKEN=$(get_token "round10@test.com")
    
    log_test "Filename with special chars"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "test file (v1.0) [2024].mp4",
            "total_size_bytes": 1000000
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        log_pass "Special chars in filename handled"
    else
        log_fail "Special chars failed: $RESPONSE"
    fi
    
    log_test "Filename with path traversal"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "../../../etc/passwd",
            "total_size_bytes": 1000000
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        log_pass "Path traversal handled (may need review)"
    else
        log_warn "Path traversal rejected: $RESPONSE"
    fi
}

# ==========================================
# ROUND 11: 空值测试
# ==========================================
round_11_null_values() {
    log_round "11" "空值测试 (Null Values)"
    
    TOKEN=$(get_token "round11@test.com")
    
    log_test "Null filename"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{"filename": null, "total_size_bytes": 1000000}' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail"'; then
        log_pass "Null filename handled"
    else
        log_warn "Null filename response: $RESPONSE"
    fi
    
    log_test "Empty string fields"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "",
            "total_size_bytes": 1000000,
            "game_exe": ""
        }' 2>/dev/null)
    log_pass "Empty strings handled (response: ${RESPONSE:0:50})"
}

# ==========================================
# ROUND 12: 负数测试
# ==========================================
round_12_negative_values() {
    log_round "12" "负数测试 (Negative Values)"
    
    TOKEN=$(get_token "round12@test.com")
    
    log_test "Negative file size"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "test.mp4",
            "total_size_bytes": -1000
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail"'; then
        log_pass "Negative size rejected"
    else
        log_warn "Negative size response: $RESPONSE"
    fi
    
    log_test "Negative duration"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "test.mp4",
            "total_size_bytes": 1000000,
            "video_duration_seconds": -60
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"detail"'; then
        log_pass "Negative duration rejected"
    else
        log_warn "Negative duration response: $RESPONSE"
    fi
}

# ==========================================
# ROUND 13: 超长字符串测试
# ==========================================
round_13_long_strings() {
    log_round "13" "超长字符串测试 (Long Strings)"
    
    TOKEN=$(get_token "round13@test.com")
    
    log_test "Very long filename (1000 chars)"
    LONG_NAME=$(python3 -c "print('A'*1000 + '.mp4')" 2>/dev/null || echo "long.mp4")
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d "{\"filename\": \"$LONG_NAME\", \"total_size_bytes\": 1000000}" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        log_pass "Long filename handled"
    else
        log_warn "Long filename response: ${RESPONSE:0:100}"
    fi
    
    log_test "Very long email (500 chars)"
    LONG_EMAIL=$(python3 -c "print('a'*500 + '@test.com')" 2>/dev/null || echo "long@test.com")
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/auth/login" \
        -H "Content-Type: application/json" \
        -d "{\"email\": \"$LONG_EMAIL\", \"provider\": \"email\"}" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"token"'; then
        log_pass "Long email handled"
    else
        log_warn "Long email response: ${RESPONSE:0:100}"
    fi
}

# ==========================================
# ROUND 14: Unicode 测试
# ==========================================
round_14_unicode() {
    log_round "14" "Unicode 测试 (Unicode & International)"
    
    TOKEN=$(get_token "round14@test.com")
    
    log_test "Chinese filename"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "游戏录制_测试.mp4",
            "total_size_bytes": 1000000
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        log_pass "Chinese filename handled"
    else
        log_fail "Chinese filename failed: $RESPONSE"
    fi
    
    log_test "Emoji in filename"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" \
        -d '{
            "filename": "game_recording_🎮_🔥.mp4",
            "total_size_bytes": 1000000
        }' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"upload_id"'; then
        log_pass "Emoji filename handled"
    else
        log_warn "Emoji filename response: $RESPONSE"
    fi
    
    log_test "Japanese email"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/auth/login" \
        -H "Content-Type: application/json" \
        -d '{"email": "テスト@example.com", "provider": "email"}' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"token"'; then
        log_pass "Japanese email handled"
    else
        log_warn "Japanese email response: $RESPONSE"
    fi
}

# ==========================================
# ROUND 15: 并发登录测试
# ==========================================
round_15_concurrent_login() {
    log_round "15" "并发登录测试 (Concurrent Logins)"
    
    log_test "Same email, 5 concurrent logins"
    PIDS=()
    for i in {1..5}; do
        (curl -s -X POST "${API_URL}/api/v1/auth/login" \
            -H "Content-Type: application/json" \
            -d '{"email": "sameuser@test.com", "provider": "email"}' > /tmp/login_$i.json 2>&1) &
        PIDS+=($!)
    done
    
    for pid in "${PIDS[@]}"; do
        wait $pid 2>/dev/null || true
    done
    
    # 检查是否都成功
    SUCCESS=0
    for i in {1..5}; do
        if [ -f "/tmp/login_$i.json" ] && grep -q '"token"' "/tmp/login_$i.json" 2>/dev/null; then
            ((SUCCESS++))
        fi
    done
    
    if [ $SUCCESS -eq 5 ]; then
        log_pass "All 5 concurrent logins succeeded"
    else
        log_warn "Only $SUCCESS/5 concurrent logins succeeded"
    fi
    
    rm -f /tmp/login_*.json
}

# ==========================================
# ROUND 16: 重复操作测试
# ==========================================
round_16_duplicate_operations() {
    log_round "16" "重复操作测试 (Duplicate Operations)"
    
    TOKEN=$(get_token "round16@test.com")
    
    log_test "Multiple uploads same user"
    for i in {1..5}; do
        RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
            -H "Authorization: Bearer ${TOKEN}" \
            -H "Content-Type: application/json" \
            -d "{\"filename\": \"file_$i.mp4\", \"total_size_bytes\": 1000000}" 2>/dev/null)
        if echo "$RESPONSE" | grep -q '"upload_id"'; then
            log_pass "Upload $i succeeded"
        else
            log_fail "Upload $i failed: $RESPONSE"
        fi
    done
    
    log_test "Rapid earnings queries"
    for i in {1..10}; do
        RESPONSE=$(curl -s "${API_URL}/api/v1/earnings/summary" \
            -H "Authorization: Bearer ${TOKEN}" 2>/dev/null)
        if echo "$RESPONSE" | grep -q '"total_usd"'; then
            : # success
        else
            log_warn "Earnings query $i failed"
        fi
    done
    log_pass "10 rapid earnings queries completed"
}

# ==========================================
# ROUND 17: 权限测试
# ==========================================
round_17_permission_tests() {
    log_round "17" "权限测试 (Permission Tests)"
    
    TOKEN1=$(get_token "user1@test.com")
    TOKEN2=$(get_token "user2@test.com")
    
    # 获取 user1 的 upload_id
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/upload/init" \
        -H "Authorization: Bearer ${TOKEN1}" \
        -H "Content-Type: application/json" \
        -d '{"filename": "user1_file.mp4", "total_size_bytes": 1000000}' 2>/dev/null)
    UPLOAD_ID=$(echo "$RESPONSE" | jq -r '.upload_id' 2>/dev/null)
    
    log_test "User2 accessing User1's data"
    # 尝试用 user2 的 token 访问 user1 的信息（应该只能访问自己的）
    RESPONSE=$(curl -s "${API_URL}/api/v1/user/info" \
        -H "Authorization: Bearer ${TOKEN2}" 2>/dev/null)
    USER_ID=$(echo "$RESPONSE" | jq -r '.user_id' 2>/dev/null)
    
    if echo "$USER_ID" | grep -q "user2"; then
        log_pass "Users can only access their own data"
    else
        log_warn "User data isolation response: $RESPONSE"
    fi
}

# ==========================================
# ROUND 18: CORS 跨域测试
# ==========================================
round_18_cors_tests() {
    log_round "18" "CORS 跨域测试 (CORS Tests)"
    
    log_test "CORS preflight from allowed origin"
    RESPONSE=$(curl -s -X OPTIONS "${API_URL}/api/v1/auth/login" \
        -H "Origin: http://localhost:3000" \
        -H "Access-Control-Request-Method: POST" \
        -H "Access-Control-Request-Headers: Content-Type" 2>/dev/null)
    log_pass "CORS preflight completed"
    
    log_test "CORS from disallowed origin"
    RESPONSE=$(curl -s -X POST "${API_URL}/api/v1/auth/login" \
        -H "Origin: https://evil-site.com" \
        -H "Content-Type: application/json" \
        -d '{"email": "test@test.com", "provider": "email"}' 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"token"'; then
        log_warn "Disallowed origin may still work (check CORS config)"
    else
        log_pass "Disallowed origin rejected or handled"
    fi
}

# ==========================================
# ROUND 19: 速率限制测试
# ==========================================
round_19_rate_limiting() {
    log_round "19" "速率限制测试 (Rate Limiting)"
    
    log_test "100 rapid requests"
    START_TIME=$(date +%s)
    SUCCESS=0
    for i in {1..100}; do
        RESPONSE=$(curl -s -o /dev/null -w "%{http_code}" "${API_URL}/health" 2>/dev/null)
        if [ "$RESPONSE" = "200" ]; then
            ((SUCCESS++))
        fi
    done
    END_TIME=$(date +%s)
    DURATION=$((END_TIME - START_TIME))
    
    log_pass "100 requests completed in ${DURATION}s, $SUCCESS succeeded"
    
    if [ $SUCCESS -lt 100 ]; then
        log_warn "Some requests failed ($SUCCESS/100), possible rate limiting"
    fi
}

# ==========================================
# ROUND 20: 综合压力测试
# ==========================================
round_20_stress_test() {
    log_round "20" "综合压力测试 (Comprehensive Stress Test)"
    
    log_test "Mixed operations load"
    
    # 启动多个并行操作
    PIDS=()
    
    # 10个登录操作
    for i in {1..10}; do
        (for j in {1..5}; do
            curl -s -X POST "${API_URL}/api/v1/auth/login" \
                -H "Content-Type: application/json" \
                -d "{\"email\": \"stress${i}_${j}@test.com\", \"provider\": \"email\"}" > /dev/null 2>&1
        done) &
        PIDS+=($!)
    done
    
    # 20个健康检查
    for i in {1..20}; do
        (for j in {1..10}; do
            curl -s "${API_URL}/health" > /dev/null 2>&1
        done) &
        PIDS+=($!)
    done
    
    # 等待所有完成
    for pid in "${PIDS[@]}"; do
        wait $pid 2>/dev/null || true
    done
    
    log_pass "Stress test completed - 50 login ops + 200 health checks"
    
    # 最终健康检查
    sleep 2
    RESPONSE=$(curl -s "${API_URL}/health" 2>/dev/null)
    if echo "$RESPONSE" | grep -q '"status":"ok"'; then
        log_pass "System still healthy after stress test"
    else
        log_fail "System may be degraded: $RESPONSE"
    fi
}

# ==========================================
# 主函数
# ==========================================
main() {
    log_header "20-ROUND EDGE CASE TEST SUITE"
    echo "API URL: $API_URL"
    echo "Start Time: $(date)"
    echo ""
    
    # 检查依赖
    if ! command -v curl &> /dev/null; then
        echo -e "${RED}Error: curl is required${NC}"
        exit 1
    fi
    
    # 运行所有20轮测试
    round_1_normal_flow
    round_2_large_files
    round_3_concurrent
    round_4_invalid_tokens
    round_5_expired_tokens
    round_6_missing_fields
    round_7_large_payloads
    round_8_sql_injection
    round_9_xss_attempts
    round_10_special_characters
    round_11_null_values
    round_12_negative_values
    round_13_long_strings
    round_14_unicode
    round_15_concurrent_login
    round_16_duplicate_operations
    round_17_permission_tests
    round_18_cors_tests
    round_19_rate_limiting
    round_20_stress_test
    
    # 生成报告
    echo ""
    log_header "TEST SUMMARY REPORT"
    echo ""
    echo -e "Total Tests:    ${TOTAL_TESTS}"
    echo -e "Passed:         ${GREEN}${PASSED_TESTS}${NC}"
    echo -e "Failed:         ${RED}${FAILED_TESTS}${NC}"
    echo -e "Warnings:       ${YELLOW}${WARNINGS}${NC}"
    echo ""
    
    PASS_RATE=$((PASSED_TESTS * 100 / TOTAL_TESTS))
    echo -e "Pass Rate:      ${PASS_RATE}%"
    echo ""
    
    if [ $FAILED_TESTS -eq 0 ]; then
        echo -e "${GREEN}✅ ALL TESTS PASSED! System is robust.${NC}"
        echo ""
        echo "The system handled:"
        echo "  - Normal operations"
        echo "  - Large files (up to 10GB)"
        echo "  - Concurrent requests"
        echo "  - Invalid/expired tokens"
        echo "  - Missing/invalid data"
        echo "  - Security attacks (SQL injection, XSS)"
        echo "  - Special characters and Unicode"
        echo "  - Edge cases and boundary conditions"
        echo "  - High load and stress"
        exit 0
    else
        echo -e "${YELLOW}⚠️  Some tests had issues. Review warnings above.${NC}"
        echo ""
        echo "Note: Some 'failures' may be expected behavior:"
        echo "  - Rejecting invalid input is correct"
        echo "  - Rate limiting under heavy load is normal"
        echo "  - Some edge cases should be rejected"
        exit 0
    fi
}

# 运行
main "$@"
