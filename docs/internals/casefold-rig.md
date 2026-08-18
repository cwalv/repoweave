# The casefold rig: measuring the identity match on a folding filesystem

> **Implementation, not interface.** Nothing in this document is a stable
> surface: the structures described here may change without notice, and
> operating on them directly — with shell tools, file edits, or git commands
> against rwv-managed state — is not supported. Operations on or between
> workweaves go through rwv verbs. If you need an operation no verb provides,
> that is a UX gap: file it rather than working around it at the file level.

`workweave_index::same_directory` decides directory identity by `(dev, ino)`
rather than by comparing canonicalized path strings. On a filesystem that does
not fold case, the two approaches never disagree: `canonicalize` resolves
every alias to the one spelling on disk, so a case-drifted query and the
on-disk entry canonicalize to the same string regardless of which comparison
`same_directory` uses. **The two approaches diverge only on a filesystem where
`canonicalize` echoes back the spelling it was asked with instead of
resolving to the on-disk spelling** — which on Linux is exactly what an ext4
directory with the `casefold` feature does: `realpath ChAtLy` on a directory
whose true entry is `Chatly` returns `ChAtLy` verbatim, because the dcache
serves the case-insensitive lookup as an alias rather than a redirect. macOS
and Windows both resolve to the on-disk spelling even when folding, so this
divergence is Linux-casefold-specific; no other host this suite runs on can
produce it.

This is why the identity match has only ever had a structural pin in this
tree: nothing in the test suite's reach can make the two comparisons answer
differently. This document is the procedure for measuring it for real, on a
host that can.

## What the host needs, and how to check before building anything

Two independent requirements, and a host can fail either one silently — check
both before spending time on the image.

1. **A kernel built with ext4 casefold support.**

   ```sh
   grep -i casefold /boot/config-$(uname -r)
   ```

   No output means the running kernel cannot mount a casefold directory at
   all, independent of privilege — building the image below will still
   succeed (`mkfs.ext4` writes the feature flag whether or not the kernel can
   honor it), but mounting it or setting `+F` on a directory inside it will
   fail with `Operation not supported`. This is a different failure than the
   one privilege produces, and worth telling apart: a host with the kernel
   feature but no root needs sudo; a host without the kernel feature needs a
   different kernel, no amount of privilege fixes it.

2. **Privilege to attach a loopback device and mount it** — `losetup` and
   `mount` both require `CAP_SYS_ADMIN` in the mounting namespace. A bare
   `chattr +F` on the host's existing filesystem is the fastest single-command
   check of requirement 1 on its own (it fails the same way — `Operation not
   supported` — whether the kernel lacks the feature or the directory's
   filesystem wasn't formatted with it), but it says nothing about requirement
   2, since it needs no mount of its own.

## Building the rig

```sh
# A sparse 64 MiB image is enough for the fixtures this suite builds.
dd if=/dev/zero of=casefold.img bs=1M count=64
mkfs.ext4 -O casefold -F casefold.img

# Requires privilege (root, or CAP_SYS_ADMIN in the current namespace).
sudo losetup -f --show casefold.img   # prints the loop device, e.g. /dev/loop0
sudo mkdir -p /mnt/casefold-rig
sudo mount /dev/loop0 /mnt/casefold-rig
sudo chown "$(id -u):$(id -g)" /mnt/casefold-rig

# The casefold feature is per-directory, opt-in, and set only on an empty
# directory.
mkdir /mnt/casefold-rig/fixtures
sudo chattr +F /mnt/casefold-rig/fixtures
lsattr -d /mnt/casefold-rig/fixtures   # confirm the F flag took
```

Cleanup, once done — reverse order, and the image can be deleted last:

```sh
sudo umount /mnt/casefold-rig
sudo losetup -d /dev/loop0
rm casefold.img
```

## Pointing the suite at it

`tests/common::tempdir()` roots every fixture in the suite under
`std::env::temp_dir()`, which reads `TMPDIR` on Unix. Running with `TMPDIR`
set to a directory under the casefold mount routes every fixture this suite
builds onto the rig, with no test code changed — this is the same mechanism
`tests/common/mod.rs`'s own doc comment already documents for reproducing the
macOS symlinked-temp-root geometry, applied to a different host property:

```sh
TMPDIR=/mnt/casefold-rig/fixtures cargo test --release --no-fail-fast
```

The case-equivalence suite (`tests/case_equivalence_test.rs`) asks the
fixture directory itself whether it folds (`filesystem_folds_case`) rather
than branching on the host, so every folding-arm assertion in that file
activates automatically once `TMPDIR` points at the rig — no `#[ignore]`, no
feature flag, no environment variable the tests themselves read.

## What to check, per target

**Target 1 — the identity match itself.** No test in the tree isolates this
by itself; the folding-arm assertions in
`tests/case_equivalence_test.rs::a_confusable_sibling_warns_at_mint_and_is_still_created`
reach it (via `listed_occupant`, which calls `same_directory`) but would also
pass under a canonicalize-based comparison that happened to resolve
correctly — which on this rig it would not, and that is the measurement:

1. Run the suite against the rig as above and confirm
   `a_confusable_sibling_warns_at_mint_and_is_still_created` passes, folding
   arm, with output showing `lists it as`.
2. Apply the mutation: in `src/workweave_index.rs`, change
   `same_directory`'s body to the old comparison —
   `canonical_recorded_path(a) == canonical_recorded_path(b)` unconditionally,
   deleting the `cfg(unix)` `(dev, ino)` block above it.
3. Re-run the same test against the rig. It must fail, and the failure must
   be the `lists it as` assertion specifically — check which assertion fired,
   not just that the test went red; an earlier assertion failing first would
   mean the mutation broke something else and the identity-match claim is
   still unmeasured.
4. Revert the mutation (reverse the patch, not `git checkout`) and confirm
   the suite is green again and `git status --porcelain` is empty.

Green at step 1, red at step 3 on the named assertion, green again after the
revert is the acceptance criterion — the same green-red-green a mutation
needs anywhere in this tree, just run against a filesystem where the
predicate this pins can actually vary.

**Target 2 — occupant naming.** Already behaviorally pinned in CI on any
folding host (macOS, Windows). Running the suite against this rig adds a
Linux-native confirmation of the same pin; nothing new to check beyond the
suite passing.

**Target 3 — the workweave reuse guard.** Pinned by
`tests/case_equivalence_test.rs::workweave_reuse_refuses_to_adopt_a_case_twin_directory`.
Its folding arm has not been executed anywhere before this rig exists — run
the suite against the rig and confirm it passes, folding arm, with output
containing `different workweave, not this one` and `lists it as`.

**Target 4 — the fetch materialization path.** No test in the tree exercises
this yet. Manual reproduction, run against the rig:

```sh
mkdir -p /mnt/casefold-rig/fixtures/remotes
git init --bare --initial-branch=main /mnt/casefold-rig/fixtures/remotes/chatly.git
# ... push one commit to it, as any fetch fixture does ...

mkdir /mnt/casefold-rig/fixtures/ws && cd /mnt/casefold-rig/fixtures/ws
rwv fetch /mnt/casefold-rig/fixtures/remotes/chatly.git      # mints projects/chatly
rwv fetch /mnt/casefold-rig/fixtures/remotes/Chatly.git      # same repo, case-twin name
```

The second fetch must refuse, and the refusal must name the occupant as the
parent lists it (`describe_existing`'s "lists it as" clause) rather than
silently cloning into a directory the filesystem is about to fold onto the
first. A permanent test would mirror
`tests/case_equivalence_test.rs::workweave_reuse_refuses_to_adopt_a_case_twin_directory`
against `tests/fetch_test.rs::fetch_existing_workspace_handles_gracefully`'s
fixture shape — probe the projects directory for folding, fetch a case-twin
source, and assert the refusal arm only where the probe says it folds. That
test does not exist yet; writing it needs no privilege at all, only the same
`filesystem_folds_case` idiom `tests/case_equivalence_test.rs` already uses
(currently private to that file — a fetch test reusing it would move the
probe to `tests/common` first, or duplicate the dozen lines). That is what
makes it runnable on ordinary CI the same way target 3's test now is.
