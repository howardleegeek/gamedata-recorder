# Rust Pre-Commit Hooks Skill

Automatically sets up git pre-commit hooks for Rust projects that validate code quality before every commit.

## Quick Start

```bash
# From your project root
bash skills/rust-pre-commit-hooks/scripts/setup-hooks.sh
```

## What It Does

The installed pre-commit hook runs before every `git commit`:

1. **Build Check**: Runs `cargo check`
   - Blocks commit if build fails
   - Shows compilation errors
   - Ensures code actually works
   - Note: Faster than cargo build (no linking)

2. **Formatting Check**: Runs `cargo fmt --check`
   - Auto-runs `cargo fmt` if formatting issues found
   - Shows what was changed
   - Only allows commit if properly formatted

## Files

- **SKILL.md**: Skill documentation
- **scripts/setup-hooks.sh**: Installation script
- **scripts/pre-commit.sh**: The actual git hook
- **evals/evals.json**: Test cases for the skill

## Usage Examples

```bash
# Normal commit - hook runs automatically
git commit -m "Add new feature"

# Bypass hook if needed (not recommended)
git commit --no-verify -m "WIP"
```

## Test Cases

The skill has been tested with these scenarios:

1. **New project setup** - User wants hooks for a new Rust project
2. **Team consistency** - Prevent formatting inconsistencies across team
3. **CI/CD complement** - Catch issues locally before remote CI fails

## Troubleshooting

**Hook not running?**
```bash
# Check if executable
ls -l .git/hooks/pre-commit
# Should show -rwxr-xr-x

# Make executable if needed
chmod +x .git/hooks/pre-commit
```

**Build too slow?**
Edit `scripts/pre-commit.sh` and replace `cargo build` with `cargo check`

**Remove hooks:**
```bash
rm .git/hooks/pre-commit
```
