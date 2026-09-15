---
name: changepacks
description: |
  Version and changelog management for multi-language monorepos. Read this before opening a pull request in any repository that has a `.changepacks/` directory.

  TRIGGER WHEN:
  - The repository has a `.changepacks/` directory
  - Opening a pull request, or preparing a commit that will become one
  - A CI check named "changepack required" (or similar) failed
  - Asked to release, bump a version, or write a changelog entry
  - Running `changepacks`, `npx @changepacks/cli`, or `bunx @changepacks/cli`

  Trigger keywords: changepack, changepacks, changelog, version bump, release,
  "changepack required", monorepo versioning, npx @changepacks/cli, bunx @changepacks/cli
---

# changepacks

A repository with a `.changepacks/` directory manages its versions with
[changepacks](https://github.com/changepacks/changepacks). In such a repository
**a pull request that changes a tracked package must add a changepack log**, or
the version never moves and the change ships to the default branch and is never
released.

## Use the non-interactive form

This is the single most important thing on this page. Running the tool with no
arguments opens an interactive selection UI:

```bash
changepacks          # interactive - prompts for projects, bump level and notes
```

In an agent shell, a CI job, or any non-TTY context that **hangs or is
cancelled**, which is the usual reason a pull request arrives without the
changepack it needed. Pass the three answers as flags instead:

```bash
bunx @changepacks/cli --yes --update-type patch --message "what changed and why"
```

| Flag | Short | Meaning |
|------|-------|---------|
| `--yes` | `-y` | Take every changed project; do not prompt for selection |
| `--update-type` | `-u` | `major`, `minor` or `patch` |
| `--message` | `-m` | The release note |
| `--filter` | `-f` | `workspace` or `package` |
| `--language` | `-l` | Restrict to one language; repeatable |

Any package manager works — use the one the repository already uses:

```bash
bunx @changepacks/cli -y -u patch -m "..."    # bun
npx  @changepacks/cli -y -u patch -m "..."    # npm
changepacks -y -u patch -m "..."              # installed binary
```

## Which changes need one

`.changepacks/config.json` decides it, not a general rule. The `ignore` array is
a list of glob patterns where a leading `!` means *tracked*:

```jsonc
// tracks only crate manifests
{ "ignore": ["**", "!/crates/*/Cargo.toml"], "baseBranch": "main" }

// tracks every package and one binding manifest
{ "ignore": ["*", "!packages/*/*", "!bindings/*/package.json"] }
```

Read that file before deciding. A documentation-only change in a repository
whose config tracks `packages/*/*` needs no changepack; the same change inside
`packages/` does.

To see what the tool itself thinks changed:

```bash
changepacks check            # list projects and their change state
changepacks check --tree     # with the dependency tree
changepacks check --remote   # compare against the remote base branch
```

## Choosing the bump

| Level | Use when |
|-------|----------|
| `major` | A consumer must change their code to upgrade |
| `minor` | New capability, existing usage keeps working |
| `patch` | Fix, correction, or an internal change that ships |

A corrected document or a re-vendored asset **is** a shipped change if the
package carries it. "It is only docs" is about the repository, not about what
users receive.

## What it produces

One file, which you commit with your change:

```
.changepacks/changepack_log_<name>.json
```

```json
{
  "changes": { "crates/my-crate/Cargo.toml": "Patch" },
  "note": "Explains what changed and why, for someone reading the changelog later.",
  "date": "2026-01-01T00:00:00+09:00"
}
```

The note becomes the changelog entry. Write the reason, not the diff — the
commit already has the diff.

## The rest of the cycle

You normally only create the log. The remaining steps are usually automated on
the default branch, and running them by hand in a pull request is wrong:

```bash
changepacks update --dry-run   # preview the version bumps a merge would apply
changepacks update             # apply them (usually CI's job, not yours)
changepacks publish            # release in dependency order (usually CI's job)
```

## If CI says a changepack is required

That check is comparing your changed paths against `.changepacks/config.json`.
Add the log and push:

```bash
bunx @changepacks/cli -y -u patch -m "<why this change exists>"
git add .changepacks && git commit -m "chore: add changepack" && git push
```

Do not satisfy the check by reverting the tracked file. The change is wanted;
the record of it is what was missing.
