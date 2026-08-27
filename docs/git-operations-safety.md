# Git Operations Safety Guide

## 🚨 Critical Rule: NEVER Use `git checkout` in User's Working Directory

### Why This Is Dangerous
- `git checkout` overwrites working directory files with target branch/commit content
- Unstaged changes are **permanently lost** - NO RECOVERY through normal git operations
- No warning prompt - happens silently and immediately
- Force flag not required - regular checkout is destructive
- Cannot rely on git reflog to recover unstaged work

## Prohibited Operations (in user's working directory)

- `git checkout <branch>` - Switching branches overwrites unstaged work
- `git checkout <commit>` - Switching commits overwrites unstaged work  
- `git checkout origin/pr/*` - Checking out remote PRs overwrites unstaged work
- `git reset --hard` - Hard reset destroys all unstaged work
- `git clean -fd` - Removes untracked files permanently
- `git switch <branch>` - Same as checkout, overwrites work
- `git merge` - May conflict with unstaged work

## Acceptable Operations (in user's working directory)

- `git status` - Safe
- `git diff` - Safe  
- `git add` - Safe
- `git commit` - Safe (commits staged work)
- `git stash` - Safe (protects unstaged work)
- `git log` - Safe
- `git branch` - Safe
- `git fetch` - Safe
- `git pull --ff-only` - Safe (fast-forward only)
- `git show` - Safe

## Mandatory Workaround Process

### 1. Check Working Directory Status
```bash
git status
# If any files show as "modified" or "new file" but NOT "staged", STOP!
```

### 2. If Unstaged Work Exists, Stash First
```bash
git stash push -m "describe work briefly"
# Now safe to do checkout/branch operations
```

### 3. Perform Git Operations in Separate Directory
```bash
cd /tmp/winuxsh-pr-review-$$
git clone <repository> .
# Now safe to check out branches, test PRs, etc.
```

### 4. Restore User's Work When Done
```bash
cd ../unixwin-winuxsh
git stash pop  # Restore user's unstaged work
```

## Alternative: Git Worktree

```bash
# Create separate worktree for testing
git worktree add ../winuxsh-pr-review origin/pr/58
cd ../winuxsh-pr-review
# Now safe to test PR without affecting original work directory
# When done:
cd ../unixwin-winuxsh
git worktree remove ../winuxsh-pr-review
```

## Emergency Recovery If Work Was Lost

### Step 1: Check for Dangling Blobs
```bash
git fsck --lost-found
# Look for "dangling blob" entries
# Example output:
# dangling blob 88c1c4592cc856945f62edf57ded11596762c3b5
# dangling blob 9afec548f15f0ee2f110b1c474aaae51f10c7d26
```

### Step 2: Examine Dangling Blobs
```bash
# Check each blob to see if it's the lost code
git show 88c1c4592cc856945f62edf57ded11596762c3b5
# If it looks like your lost code, save it:
git show 88c1c4592cc856945f62edf57ded11596762c3b5 > path/to/file
```

### Step 3: Check Git Reflog
```bash
git reflog -20
# Look for checkout operations that may have overwritten work
# Example output:
# 62fc1f6 HEAD@{Thu Aug 27 11:59:11 2026}: checkout: moving from master to feature/history-mode-configuration
# 62fc1f6 HEAD@{Thu Aug 27 11:58:45 2026}: checkout: moving from temp-pr58 to master
```

### Step 4: Check All Branches
```bash
git branch -a
# Check if your work might be in a different branch
git checkout <other-branch>
git diff master
```

## Pre-PR Review Checklist

- [ ] Ran `git status` and confirmed clean working directory
- [ ] If work existed, ran `git stash push -m "description"`
- [ ] Created separate test directory for PR testing
- [ ] Cloned repo to test directory or used `git worktree`
- [ ] Did NOT execute any `git checkout` commands in original directory
- [ ] After testing, restored user work with `git stash pop`

## Zero-Tolerance Policy

Any agent that executes `git checkout` in a user's active working directory with unstaged work is immediately considered **BROKEN** and requires emergency patch.

This is a catastrophic data loss scenario that cannot be recovered through normal Git operations.

## Example of What Went Wrong (Historical Case)

### The Mistake
Agent executed these operations in user's working directory with unstaged work:
```bash
git checkout master                                    # Overwrote unstaged work
git checkout feature/history-mode-configuration        # Overwrote unstaged work  
git checkout origin/pr/58                              # Overwrote unstaged work
git checkout master                                    # Overwrote unstaged work
git checkout origin/pr/55                              # Overwrote unstaged work
```

### The Result
User's unstaged changes to `src/main.rs` and `tests/host_contract.rs` were permanently lost through 13 consecutive checkout operations.

### The Correct Approach That Should Have Been Used
```bash
# Step 1: Check status first
git status
# Output showed: modified: src/main.rs, tests/host_contract.rs

# Step 2: Protect user's work
git stash push -m "user's feature work"

# Step 3: Test PR in separate directory
cd /tmp/pr-review-$$
git clone <repository> .
git checkout origin/pr/58
# ... perform PR review ...

# Step 4: Return and restore work
cd ../unixwin-winuxsh
git stash pop
```
