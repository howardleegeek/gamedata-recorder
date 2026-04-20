# Git pre-commit hook for Rust projects (PowerShell version)
# Runs cargo build and cargo fmt before allowing commit
#

Write-Host "Running pre-commit hook..." -ForegroundColor Yellow

$ErrorActionPreference = "Continue"

$TmpFile = [System.IO.Path]::GetTempFileName()

function Run-Check {
    param(
        [string]$Name,
        [string]$Command,
        [string]$FixCommand = ""
    )

    Write-Host "  $Name... " -NoNewline

    $Output = Invoke-Expression $Command 2>&1 | Out-String

    if ($LASTEXITCODE -eq 0) {
        Write-Host "✓ passed" -ForegroundColor Green
        return $true
    } else {
        Write-Host "✗ failed" -ForegroundColor Red
        Write-Host $Output

        if ($FixCommand -ne "") {
            Write-Host "Attempting to fix..." -ForegroundColor Yellow
            $FixOutput = Invoke-Expression $FixCommand 2>&1 | Out-String

            if ($LASTEXITCODE -eq 0) {
                Write-Host "✓ Fixed!" -ForegroundColor Green

                # Recheck after fixing
                $RecheckOutput = Invoke-Expression $Command 2>&1 | Out-String
                if ($LASTEXITCODE -eq 0) {
                    return $true
                } else {
                    Write-Host "✗ Still failing after fix attempt" -ForegroundColor Red
                    Write-Host $RecheckOutput
                    return $false
                }
            } else {
                Write-Host "✗ Fix attempt failed" -ForegroundColor Red
                Write-Host $FixOutput
                return $false
            }
        }

        return $false
    }
}

# Run checks
$BuildFailed = $false
$FormatFailed = $false

# Check 1: Build (using cargo check to avoid linker stack overflow on Windows)
if (-not (Run-Check "Build check" "cargo check")) {
    $BuildFailed = $true
}

# Check 2: Formatting (with auto-fix)
if (-not (Run-Check "Formatting check" "cargo fmt --check" "cargo fmt")) {
    $FormatFailed = $true
}

# Cleanup
if (Test-Path $TmpFile) {
    Remove-Item $TmpFile -Force
}

# Summary
Write-Host ""

if ($BuildFailed -or $FormatFailed) {
    Write-Host "=== Pre-commit hook failed ===" -ForegroundColor Red
    Write-Host ""
    Write-Host "Your commit was blocked due to the errors above."

    if ($BuildFailed) {
        Write-Host ""
        Write-Host "Build errors:" -ForegroundColor Yellow
        Write-Host "  - Fix compilation errors"
        Write-Host "  - Run 'cargo build' to see full errors"
        Write-Host "  - Ensure all dependencies are in Cargo.toml"
    }

    if ($FormatFailed) {
        Write-Host ""
        Write-Host "Formatting issues:" -ForegroundColor Yellow
        Write-Host "  - Auto-fix attempted with 'cargo fmt'"
        Write-Host "  - Review the changes with 'git diff'"
        Write-Host "  - Stage the formatting fixes: 'git add -u'"
        Write-Host "  - Then commit again"
    }

    Write-Host ""
    Write-Host "To bypass this hook (not recommended):"
    Write-Host "  git commit --no-verify -m `"your message"`"
    Write-Host ""

    exit 1
}

Write-Host "=== All checks passed ===" -ForegroundColor Green
Write-Host ""
Write-Host "Proceeding with commit..."
exit 0
