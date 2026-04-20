# PowerShell setup script for Rust pre-commit hooks
# This script installs git pre-commit hooks that run cargo build and cargo fmt
#

Write-Host "=== Rust Pre-Commit Hooks Setup ===" -ForegroundColor Cyan
Write-Host ""

# Check if we're in a git repository
if (-not (Test-Path ".git")) {
    Write-Host "Error: .git directory not found" -ForegroundColor Red
    Write-Host "Please run this script from the root of your git repository."
    exit 1
}

# Check if Cargo.toml exists (Rust project check)
if (-not (Test-Path "Cargo.toml")) {
    Write-Host "Warning: Cargo.toml not found" -ForegroundColor Yellow
    Write-Host "This doesn't appear to be a Rust project root."
    $response = Read-Host "Continue anyway? (y/n)"
    if ($response -ne "y" -and $response -ne "Y") {
        Write-Host "Setup cancelled."
        exit 0
    }
}

# Determine the script's directory
$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$hookSource = Join-Path $scriptDir "pre-commit.ps1"
$hookTarget = ".git\hooks\pre-commit"

# Check if source hook script exists
if (-not (Test-Path $hookSource)) {
    Write-Host "Error: Hook source script not found at $hookSource" -ForegroundColor Red
    Write-Host "Please ensure the skills/rust-pre-commit-hooks/scripts/ directory exists."
    exit 1
}

# Create .git/hooks directory if it doesn't exist
$hooksDir = ".git\hooks"
if (-not (Test-Path $hooksDir)) {
    New-Item -ItemType Directory -Path $hooksDir -Force | Out-Null
    Write-Host "Created .git/hooks/ directory" -ForegroundColor Green
}

# Check if hook already exists
if (Test-Path $hookTarget) {
    Write-Host "Warning: Pre-commit hook already exists at $hookTarget" -ForegroundColor Yellow
    Write-Host ""
    Write-Host "Current hook will be backed up to: ${hookTarget}.backup"
    Copy-Item $hookTarget "${hookTarget}.backup"
    Write-Host ""
}

# Copy the hook
Write-Host "Installing pre-commit hook..."
Copy-Item $hookSource $hookTarget

# Note: PowerShell scripts don't need +x permission on Windows
# Git for Windows will execute .ps1 hooks automatically

Write-Host ""
Write-Host "✓ Pre-commit hook installed successfully" -ForegroundColor Green
Write-Host ""
Write-Host "Hook location: $hookTarget"
Write-Host ""
Write-Host "The hook will now run before every commit:"
Write-Host "  1. Runs 'cargo build' - blocks commit if build fails"
Write-Host "  2. Runs 'cargo fmt --check' - auto-fixes formatting issues"
Write-Host ""
Write-Host "To bypass the hook temporarily:"
Write-Host "  git commit --no-verify -m `"your message"`"
Write-Host ""
Write-Host "To remove the hook:"
Write-Host "  rm $hookTarget"
