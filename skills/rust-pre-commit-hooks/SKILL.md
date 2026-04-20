---
name: rust-pre-commit-hooks
description: Automatically set up git pre-commit hooks for Rust projects that run cargo build and cargo fmt before every commit, blocking commits that fail build or have formatting issues. Use this skill when the user mentions git hooks, pre-commit hooks, cargo checks before commit, ensuring code quality on commits, or automating Rust project validation. This is essential for Rust developers who want to catch build errors and formatting issues before pushing code.
compatibility: Requires git and cargo. Works on Windows (PowerShell or Git Bash), macOS, and Linux.
---

# Rust Pre-Commit Hooks Setup

This skill sets up automatic git pre-commit hooks for your Rust project to ensure every commit is:
- **Buildable**: `cargo build` succeeds
- **Well-formatted**: Code follows Rust standard formatting

## What This Does

1. **Installs a git pre-commit hook** at `.git/hooks/pre-commit`
2. **The hook runs automatically** before each commit:
   - Runs `cargo check` - blocks commit if build fails (faster than cargo build)
   - Runs `cargo fmt --check` - if formatting issues found, runs `cargo fmt` to auto-fix
   - Only allows commit if both checks pass

## When to Use This

- **New Rust project**: Set up hooks immediately after `cargo new`
- **Existing project**: Add hooks to catch future issues
- **Team project**: Ensure all contributors follow formatting standards
- **CI/CD complement**: Catch issues locally before CI fails

## Setup Instructions

### Windows (PowerShell) - Recommended

From PowerShell in your project root (where Cargo.toml is):

```powershell
powershell -ExecutionPolicy Bypass -File skills/rust-pre-commit-hooks/scripts/setup-hooks.ps1
```

Or just double-click `setup-hooks.ps1` in File Explorer.

### macOS/Linux (Bash)

From your project root:

```bash
bash skills/rust-pre-commit-hooks/scripts/setup-hooks.sh
```

### Manual Setup (Any Platform)

If you prefer to set up manually:

**Windows:**
```powershell
# Copy the PowerShell hook
copy skills\rust-pre-commit-hooks\scripts\pre-commit.ps1 .git\hooks\pre-commit
```

**macOS/Linux:**
```bash
# Copy the bash hook
cp skills/rust-pre-commit-hooks/scripts/pre-commit.sh .git/hooks/pre-commit

# Make it executable
chmod +x .git/hooks/pre-commit
```

Git for Windows will automatically detect and use `.ps1` hooks on Windows, while Unix systems use the `.sh` version.

## How It Works

When you run `git commit`:

1. **Pre-commit hook runs** automatically
2. **Build check**: `cargo check` is executed (faster than cargo build, catches same errors)
   - ✅ Success: continues to formatting check
   - ❌ Failure: commit blocked, errors shown
3. **Formatting check**: `cargo fmt --check` is executed
   - ✅ Success: commit proceeds
   - ❌ Failure: runs `cargo fmt` to fix, rechecks, then proceeds
4. **Commit completes** only if all checks pass

## What Gets Checked

### Build Check (`cargo check`)
- Syntax errors
- Missing dependencies
- Compilation errors
- Most build warnings
- Note: Faster than `cargo build`, catches same issues (no linking)

### Formatting Check (`cargo fmt`)
- Indentation (4 spaces, not tabs)
- Line length (100 char preferred)
- Trailing whitespace
- Blank line normalization
- Import ordering
- Consistent style across team

## Troubleshooting

### Hook doesn't run

**Windows:**
- Git for Windows should auto-detect `.ps1` hooks
- Check the file exists: `Test-Path .git\hooks\pre-commit`
- Try running PowerShell as Administrator if needed

**macOS/Linux:**
- Check if file is executable: `ls -l .git/hooks/pre-commit`
- Should show `-rwxr-xr-x` (executable)
- If not, run: `chmod +x .git/hooks/pre-commit`

### PowerShell execution policy error

If you get "execution of scripts is disabled on this system":

```powershell
# Option 1: Bypass for this script
powershell -ExecutionPolicy Bypass -File skills/rust-pre-commit-hooks/scripts/setup-hooks.ps1

# Option 2: Allow scripts permanently (run as Administrator)
Set-ExecutionPolicy RemoteSigned -Scope CurrentUser
```

### Build takes too long
- For large projects, consider using `cargo check` instead
- **Windows:** Edit `scripts/pre-commit.ps1`, replace `cargo build` with `cargo check`
- **macOS/Linux:** Edit `scripts/pre-commit.sh`, replace `cargo build` with `cargo check`

### Want to bypass hook temporarily
```bash
# Skip the hook for one commit
git commit --no-verify -m "WIP: work in progress"
```

### Hook blocks legitimate commit
- Fix the build errors shown
- The hook will show you exactly what's wrong
- Run `cargo build` yourself to see errors
- Run `cargo fmt` to fix formatting
- Try committing again

### Hook script permissions error (Windows)
If you get "Permission denied" on Windows Git Bash:
```bash
# Open Git Bash as Administrator
# Or run:
chmod +x .git/hooks/pre-commit
```

## Customization

### Change what gets checked

**Windows (PowerShell):** Edit `scripts/pre-commit.ps1`
**macOS/Linux (Bash):** Edit `scripts/pre-commit.sh`

```bash
# Add clippy (linter)
cargo clippy -- -D warnings

# Add tests
cargo test --lib

# Check specific crate
cargo build -p your_crate_name
```

### Adjust formatting behavior

The hook auto-runs `cargo fmt` if formatting issues are found. To disable auto-fix:

**Windows:**
```powershell
# In pre-commit.ps1, change:
cargo fmt
# To:
Write-Host "Formatting issues found. Run 'cargo fmt' to fix."
exit 1
```

**macOS/Linux:**
```bash
# In pre-commit.sh, change:
cargo fmt
# To:
echo "Formatting issues found. Run 'cargo fmt' to fix."
exit 1
```

## Removal

To remove the hooks later:

**Windows:**
```powershell
rm .git\hooks\pre-commit
```

**macOS/Linux:**
```bash
rm .git/hooks/pre-commit
```

Or disable temporarily:

```bash
git commit --no-verify -m "message"
```

## Team Usage

When working in a team:

1. **Add the script to your repo** (commit `skills/` directory)
2. **Document in CONTRIBUTING.md** that hooks are required
3. **Teammates run setup** once after cloning
4. **Everyone's code is consistently formatted**
5. **CI/CD becomes faster** (fewer formatting fix commits)

## Example Output

### Successful commit:
```
$ git commit -m "Add new feature"
Running pre-commit hook...
✓ Build check passed
✓ Formatting check passed
[master abc1234] Add new feature
```

### Build failed:
```
$ git commit -m "Add new feature"
Running pre-commit hook...
✗ Build check failed:
error[E0425]: cannot find function `foo` in this scope
  --> src/main.rs:15:5
   |
15 |     foo();
   |     ^^^ not found in this scope

Commit blocked. Please fix the errors above.
```

### Formatting fixed:
```
$ git commit -m "Add new feature"
Running pre-commit hook...
✓ Build check passed
Formatting issues found. Auto-fixing...
✓ Formatting fixed
[main def4567] Add new feature
```
