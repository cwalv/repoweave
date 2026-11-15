#!/usr/bin/env bash
# Runs the command blocks of the front-door pages and the how-to recipes
# against a built rwv in a hermetic sandbox.
# Usage: front-door-recipes.sh <dir holding rwv> [page...]
# Every `step` line must appear verbatim in its page; a page edit that drifts
# from this script fails here before anything runs.
set -uo pipefail

BIN=${1:?directory holding rwv}
shift || true
PAGES=${*:-tutorial readme release-line create-a-project add-a-repo migrate bring-home each pr-review stops lineage}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
README=$ROOT/README.md
TUTORIAL=$ROOT/docs/tutorial.md
RELEASE=$ROOT/docs/how-to/maintain-a-release-line.md
HOWTO=$ROOT/docs/how-to

FAILS=0
PENDING=0
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

# step <page file> <line exactly as written in the page>
step() {
  local page=$1 line=$2
  grep -qxF -- "$line" "$page" || { fail "not in $(basename "$page"): $line"; return 1; }
  echo "+ $line"
  eval "$line" || { fail "exit $?: $line"; return 1; }
}
# sub <page file> <page line> <concrete command run in its place>
sub() {
  local page=$1 line=$2 cmd=$3
  grep -qxF -- "$line" "$page" || { fail "not in $(basename "$page"): $line"; return 1; }
  echo "+ $cmd   (page: $line)"
  eval "$cmd" || { fail "exit $?: $cmd"; return 1; }
}
# pending <page file> <line>: spelling awaiting another change; checked present, not run
pending() {
  local page=$1 line=$2
  grep -qxF -- "$line" "$page" || { fail "not in $(basename "$page"): $line"; return 1; }
  echo "PENDING: $line"
  PENDING=$((PENDING + 1))
}
# expect <description> <command...>: a claim the page makes about the state
expect() {
  local what=$1; shift
  "$@" >/dev/null 2>&1 || fail "claim not met: $what"
}
# says <description> <needle> <command...>: output of the command holds needle
says() {
  local what=$1 needle=$2; shift 2
  local out
  out=$("$@" 2>&1)
  grep -qF -- "$needle" <<<"$out" || fail "claim not met: $what (wanted: $needle; got: $out)"
}


case "$(command -v mktemp)" in "") exit 2 ;; esac
export GIT_TERMINAL_PROMPT=0 GIT_CONFIG_NOSYSTEM=1
export PATH=$BIN:$PATH
[ "$(command -v rwv)" = "$BIN/rwv" ] || { echo "rwv on PATH is not $BIN/rwv" >&2; exit 2; }

teammate_lands() { # teammate_lands <name>: a teammate lands a server change and publishes it
  local dir=$SANDBOX/teammate
  if [ ! -d "$dir/projects" ]; then
    mkdir -p "$dir" && (cd "$dir" && rwv clone github/chatly/web-app >/dev/null 2>&1) || fail "teammate clone"
  fi
  (cd "$dir" && rwv workweave create "t-$1" >/dev/null 2>&1 \
   && cd "$SANDBOX/.workweaves/web-app--t-$1/github/chatly/server" && echo "$1" > "$1.txt" && git add -A && git commit -qm "server: $1" \
   && cd ../../.. && rwv land --retire >/dev/null 2>&1) || fail "teammate land $1"
  (cd "$dir" && rwv push >/dev/null 2>&1) || fail "teammate push $1"
}

# block <page> <first line>: the lines of the fenced block that opens with that line
block() {
  awk -v first="$2" -v nth="${3:-1}" '
    !on && $0 == first && ++seen == nth { on = 1; print; next }
    on && /^```/ { exit }
    on { print }' "$1"
}
# run <page> <first line>: run that block's lines verbatim, in order, in this shell
run() {
  local page=$1 first=$2 nth=${3:-1} line n=0
  while IFS= read -r line; do
    n=$((n + 1))
    echo "+ $line"
    eval "$line" || { fail "exit $?: $line"; return 1; }
  done < <(block "$page" "$first" "$nth")
  [ "$n" -gt 0 ] || fail "no block opens with: $first"
}
# run_stops <page> <first line>: as run, the last line is expected to exit non-zero
run_stops() {
  local page=$1 first=$2 line last
  last=$(block "$page" "$first" | tail -n 1)
  while IFS= read -r line; do
    echo "+ $line"
    if [ "$line" = "$last" ]; then eval "$line" && fail "expected a stop: $line"; else eval "$line" || { fail "exit $?: $line"; return 1; }; fi
  done < <(block "$page" "$first")
}

recipe_tutorial() {
  local page=$TUTORIAL
  run "$page" 'export SANDBOX=$(mktemp -d) && export HOME=$SANDBOX/home && mkdir -p $HOME'
  case "$HOME" in "$SANDBOX"/*) ;; *) fail "HOME not redirected"; return 1 ;; esac
  [ "$(rwv -C "$SANDBOX/seed" resolve)" = "$SANDBOX/seed" ] || { fail "seed does not resolve to itself"; return 1; }
  run "$page" 'mkdir ~/work && cd ~/work'
  run "$page" 'rwv status'
  teammate_lands first
  run "$page" 'rwv pull'
  expect "pull brings in the teammate's change" test -f "$HOME/work/github/chatly/server/first.txt"
  run "$page" 'rwv workweave create welcome-message'
  run "$page" 'cd github/chatly/protocol'
  run "$page" 'rwv log --against main'
  teammate_lands second
  run "$page" '(cd ~/work && rwv pull && rwv branch set-history linear)'
  run "$page" 'rwv sync'
  run "$page" 'rwv land --retire'
  run "$page" 'rwv push'
  run "$page" 'rwv workweave create alpha && rwv workweave create beta'
  run_stops "$page" 'cd ../.workweaves/web-app--beta'
  run "$page" 'rwv abort'
  cd ~/work && rwv workweave delete beta
  run "$page" 'rwv doctor'
}

land_on_main() { # land_on_main <name>: a change lands on main and is published
  (cd ~/work && rwv workweave create "$1" >/dev/null 2>&1 \
   && cd "$HOME/.workweaves/web-app--$1/github/chatly/server" && echo "$1" > "$1.txt" && git add -A && git commit -qm "server: $1" \
   && cd ../../.. && rwv land --retire >/dev/null 2>&1 && cd ~/work && rwv push >/dev/null 2>&1) || fail "land_on_main $1"
}

recipe_release_line() {
  local page=$RELEASE
  run "$TUTORIAL" 'export SANDBOX=$(mktemp -d) && export HOME=$SANDBOX/home && mkdir -p $HOME'
  run "$TUTORIAL" 'mkdir ~/work && cd ~/work'
  rwv branch set-history linear >/dev/null 2>&1 || fail "history"
  run "$page" 'rwv branch create stable --declared --set-new-branch github/chatly/server=v1 --set-new-branch github/chatly/protocol=protov1'
  says "stable is recorded declared" "stable                   declared" rwv branch list
  (cd ~/work && rwv switch main >/dev/null 2>&1) || fail "switch main"
  land_on_main moved-on
  run "$page" 'rwv switch main'
  expect "stable is checked out" grep -q "stable" <(rwv status 2>&1)
  run "$page" 'rwv workweave create hotfix --new-branch hotfix1 --target stable'
  run "$page" 'cd github/chatly/server'
  run "$page" 'rwv push --branch stable'
  expect "the hotfix is published on v1" test "$(git -C github/chatly/server show origin/v1:hotfix.txt)" = fixed
  run "$page" 'rwv switch main' 2
  expect "the hotfix reached main" test "$(git -C github/chatly/server show main:hotfix.txt)" = fixed
  run "$page" 'rwv switch stable' 2
  run "$page" 'rwv fetch'
  expect "v1 holds main's commit" git -C github/chatly/server cat-file -e origin/v1:moved-on.txt
  rwv switch main >/dev/null 2>&1
  run_stops "$page" 'rwv branch delete stable --target main'
  run "$page" 'rwv branch set-mode stable feature --target main'
  expect "stable is gone" test -z "$(rwv branch list 2>&1 | grep ' stable ')"
  expect "v1 stays" git -C github/chatly/server rev-parse --verify -q v1
  [ -z "${EXPLORE:-}" ] || . "$EXPLORE"
}

recipe_readme() {
  local page=$README r
  run "$TUTORIAL" 'export SANDBOX=$(mktemp -d) && export HOME=$SANDBOX/home && mkdir -p $HOME'
  for r in tooling shared-types; do git init -q --bare "$SANDBOX/remotes/chatly/$r.git"; done
  git clone -q https://github.com/chatly/shared-types.git "$SANDBOX/work-shared-types" 2>/dev/null
  git -C "$SANDBOX/work-shared-types" commit -q --allow-empty -m "initial shared-types" && git -C "$SANDBOX/work-shared-types" push -q origin main || fail "seed shared-types"
  mkdir "$SANDBOX/rd1" "$SANDBOX/rd2"
  cd "$SANDBOX/rd1" || exit 2
  run "$page" 'mkdir my-workspace && cd my-workspace'
  expect "members at their pins" test -d github/chatly/server
  run "$page" 'rwv workweave create payments'
  cd "$SANDBOX/rd1/my-workspace" || exit 2
  expect "the landing retired the workweave" test ! -d "$SANDBOX/rd1/.workweaves/web-app--payments"
  run "$page" 'rwv pull'
  cd "$SANDBOX/rd2" || exit 2
  run "$page" 'mkdir my-workspace && cd my-workspace' 2
  expect "Day 0 project published" git -C "$SANDBOX/remotes/chatly/tooling.git" rev-parse --verify -q main
}

# org <name>: https://github.com/<name>/ is served from local bare repositories too
org() { git config --global url."file://$SANDBOX/remotes/$1/".insteadOf "https://github.com/$1/"; }
# remote <org>/<repo> [file]: a bare remote holding one commit on main
remote() {
  local w=$SANDBOX/seedwork/$1
  git init -q --bare "$SANDBOX/remotes/$1.git" && mkdir -p "$(dirname "$w")" \
    && git clone -q "https://github.com/$1.git" "$w" 2>/dev/null || { fail "remote $1"; return 1; }
  [ -z "${2:-}" ] || { echo "$2" > "$w/$2" && git -C "$w" add -A; }
  git -C "$w" commit -q --allow-empty -m "initial $1" && git -C "$w" push -q origin main || fail "seed $1"
}
# fresh_project_remote: the project remote the setup block published to, emptied
fresh_project_remote() {
  rm -rf "$SANDBOX/remotes/chatly/web-app.git" && git init -q --bare "$SANDBOX/remotes/chatly/web-app.git"
}
# sandbox: the tutorial's setup block
sandbox() { run "$TUTORIAL" 'export SANDBOX=$(mktemp -d) && export HOME=$SANDBOX/home && mkdir -p $HOME'; }
# commit_in <checkout> <file> <subject>
commit_in() { echo "$3" > "$1/$2" && git -C "$1" add -A && git -C "$1" commit -qm "$3" || fail "commit in $1"; }
# stops <description> <command...>: the command exits 1 and its output is kept in $OUT
stops() {
  local what=$1; shift
  OUT=$("$@" 2>&1)
  [ $? -eq 1 ] || fail "expected exit 1: $what (got: $OUT)"
}
# printed <needle>: the line of $OUT holding needle, trimmed, as a command to run verbatim
printed() { grep -F -- "$1" <<<"$OUT" | head -n 1 | sed 's/^[[:space:]]*//'; }
# run_printed <needle>: run the command $OUT printed, as printed
run_printed() {
  local cmd; cmd=$(printed "$1")
  [ -n "$cmd" ] || { fail "not printed: $1 (in: $OUT)"; return 1; }
  echo "+ $cmd   (printed)"
  eval "$cmd" || { fail "exit $?: $cmd"; return 1; }
}

recipe_create_a_project() {
  local page=$HOWTO/create-a-project.md
  sandbox
  fresh_project_remote
  for r in client shared-types billing; do remote "chatly/$r"; done
  run "$page" 'mkdir -p ~/weave && cd ~/weave'
  expect "init commits exactly its three files" test "$(git -C projects/web-app show --format= --name-only "$(git -C projects/web-app rev-list --max-parents=0 HEAD)" | sort | tr '\n' ' ')" = ".gitattributes rwv.toml web-app.code-workspace "
  expect "init activates the project" test "$(cat .rwv-active)" = web-app
  expect "each add committed its pin" test -z "$(git -C projects/web-app status --short)"
  block "$page" '[integrations.npm-workspaces]' >> projects/web-app/rwv.toml
  echo 'default:' > projects/web-app/justfile
  run "$page" 'rwv doctor --fix'
  expect "the static file is surfaced" test -L justfile
  git -C projects/web-app add -A && git -C projects/web-app commit -qm "integrations" || fail "commit the edit"
  run "$page" 'rwv member set-url projects/web-app https://github.com/chatly/web-app.git'
  expect "the project is published" git -C "$SANDBOX/remotes/chatly/web-app.git" rev-parse --verify -q main
  (export HOME=$SANDBOX/teammate-home && mkdir -p "$HOME" && cp "$SANDBOX/home/.gitconfig" "$HOME/" \
    && run "$page" 'mkdir -p ~/weave && cd ~/weave' 2 && test -d github/chatly/client) || fail "teammate clone"
  cd ~/weave || exit 2
  run "$page" 'rwv init billing-service'
  expect "init makes the new project active" test "$(cat .rwv-active)" = billing-service
  expect "both projects list the shared member" grep -q shared-types projects/web-app/rwv.toml projects/billing-service/rwv.toml
  expect "the shared clone is one checkout" test "$(git -C github/chatly/shared-types rev-parse --git-dir)" = .git
}

recipe_add_a_repo() {
  local page=$HOWTO/add-a-repo.md lib
  sandbox
  org example; org interesting
  remote example/some-lib a.txt; remote interesting/library b.txt
  lib=$SANDBOX/seedwork/example/some-lib
  git -C "$lib" switch -qc dev && git -C "$lib" commit -q --allow-empty -m dev && git -C "$lib" push -q origin dev || fail "seed dev"
  run "$TUTORIAL" 'mkdir ~/work && cd ~/work'
  run "$page" 'rwv member add https://github.com/example/some-lib.git --role dependency'
  expect "the add committed the entry with its first pin" test -z "$(git -C projects/web-app status --short)"
  run "$page" 'rwv member add https://github.com/interesting/library.git --role reference'
  expect "a reference member's tracked files are read-only" test ! -w github/interesting/library/b.txt
  run "$page" 'rwv member set-role github/example/some-lib reference'
  run "$page" 'rwv fetch'
  says "the member sits on the branch it now declares" "dev" git -C github/example/some-lib branch --show-current
  step "$page" 'rwv member add github/chatly/auth --new'
  mkdir -p "$SANDBOX/srv/repos"
  sub "$page" 'rwv member add local/chatly/auth --new --param root=/srv/repos' "rwv member add local/chatly/auth --new --param root=$SANDBOX/srv/repos"
  stops "a plain path is refused" rwv member add "$SANDBOX/remotes/example/some-lib.git"
  grep -qF 'rwv explain unresolvable-repo-source' <<<"$OUT" || fail "claim not met: a plain path refuses as unresolvable-repo-source"
  run "$page" 'rwv member remove github/example/some-lib'
  expect "a removal leaves the checkout" test -d github/example/some-lib
  mkdir -p ~/some-other-place && mv github/example/some-lib ~/some-other-place/some-lib || fail "move the clone away"
  run "$page" 'mkdir -p github/example'
  run "$page" 'rwv member remove github/example/some-lib --delete'
  expect "--delete removes the checkout" test ! -e github/example/some-lib
  expect "every verb committed what it did" test -z "$(git -C projects/web-app status --short)"
}

recipe_migrate() {
  local page=$HOWTO/migrate-from-submodules.md
  sandbox
  fresh_project_remote
  org vendor
  remote chatly/client; remote vendor/codec
  run "$page" 'mkdir -p ~/weave && cd ~/weave'
  expect "the project is published" git -C "$SANDBOX/remotes/chatly/web-app.git" rev-parse --verify -q main
  printf '#!/bin/sh\nprintf "\\n[integrations.static-files]\\nenabled = true\\nfiles = [\\"justfile\\"]\\n" >> "$1"\n' > "$SANDBOX/enable-static-files" && chmod +x "$SANDBOX/enable-static-files"
  export EDITOR=$SANDBOX/enable-static-files
  echo 'default:' > projects/web-app/justfile
  run "$page" '$EDITOR projects/web-app/rwv.toml   # enable [integrations.<name>]'
  expect "the lock commit carried the edit and the files it declares" test -z "$(git -C projects/web-app status --short)"
  expect "the surfaced file is committed" git -C projects/web-app cat-file -e HEAD:justfile
  run "$page" 'rwv member set-url projects/web-app https://github.com/chatly/web-app.git' 2
}

recipe_bring_home() {
  local page=$HOWTO/bring-workweave-work-home.md ww
  sandbox
  run "$TUTORIAL" 'mkdir ~/work && cd ~/work'
  ww=$HOME/.workweaves
  for n in first ww1 ww2; do rwv workweave create $n >/dev/null 2>&1 || fail "create $n"; done
  commit_in "$ww/web-app--first/github/chatly/server" first.txt "server: first"
  commit_in "$ww/web-app--ww1/github/chatly/server" one.txt "server: one"
  commit_in "$ww/web-app--ww2/github/chatly/server" two.txt "server: two"
  (cd "$ww/web-app--first" && rwv land --retire >/dev/null 2>&1) || fail "a first landing fast-forwards with no history setting"
  (cd "$ww/web-app--ww1" && stops "a later landing with no history setting" rwv land --retire \
    && grep -qF 'rwv explain history' <<<"$OUT") || fail "claim not met: a target that records nothing refuses as history unset"
  run "$page" 'rwv branch set-history linear'
  (cd "$ww/web-app--ww2" && run "$page" 'rwv log --against main            # commit listing per repo beyond main'"'"'s pins')
  run "$page" '# 1. Land and retire ww1'
  cd ~/work || exit 2
  expect "both workweaves are retired" test -z "$(rwv workweave list 2>/dev/null)"
  expect "both landings are on main" test -f github/chatly/server/one.txt -a -f github/chatly/server/two.txt
  rwv workweave create payments >/dev/null 2>&1 || fail "create payments"
  touch "$ww/web-app--payments/github/chatly/web/untracked.txt"
  (cd "$ww/web-app--payments" && stops "--retire over an untracked file" rwv land --retire \
    && grep -qF 'rwv explain edits' <<<"$OUT") || fail "claim not met: --retire refuses an untracked file before anything moves"
  rm "$ww/web-app--payments/github/chatly/web/untracked.txt"
  run "$page" 'cd ../.workweaves/web-app--payments'
  expect "the landing retired the workweave" test ! -d "$ww/web-app--payments"
}

recipe_each() {
  local page=$HOWTO/run-a-command-across-repos.md
  sandbox
  run "$TUTORIAL" 'mkdir ~/work && cd ~/work'
  echo x > github/chatly/web/notes.txt
  says "each prefixes a repo's output with its path" "[github/chatly/web] ?? notes.txt" rwv each -- git status --short
  run "$page" 'rwv each -- git status --short'
  step "$page" 'rwv each -j 4 -- git fetch --all         # at most 4 repos at once (default: min(nproc, 8))'
  step "$page" 'rwv each --dry-run -- git push           # show which repos would run what, and run nothing'
  step "$page" 'rwv each --json -- git rev-parse HEAD    # one record per repo, output captured'
}

recipe_pr_review() {
  local page=$HOWTO/review-pr-with-workweave.md s
  sandbox
  remote chatly/lib lib.txt
  s=$SANDBOX/work-server
  git -C "$s" switch -qc pr-branch && commit_in "$s" pr.txt "the PR" && git -C "$s" push -q origin HEAD:refs/pull/42/head || fail "seed the PR"
  run "$TUTORIAL" 'mkdir ~/work && cd ~/work'
  rwv member add https://github.com/chatly/lib.git >/dev/null 2>&1 && rwv push >/dev/null 2>&1 || fail "add lib"
  run "$page" 'rwv workweave create pr-42'
  stops "a review workweave does not land" rwv land
  grep -qF 'alignment detached with unpushed commits' <<<"$OUT" || fail "claim not met: land refuses the detached PR checkout (got: $OUT)"
  OUT=$(rwv doctor 2>&1); [ $? -eq 1 ] || fail "doctor exits 1 in a review workweave"
  grep -qF 'pin/unpinned' <<<"$OUT" && grep -qF 'alignment/detached-with-unpushed-commits' <<<"$OUT" || fail "claim not met: doctor's two findings (got: $OUT)"
  run "$page" 'rwv log --against main            # commit listing per repo beyond main'"'"'s pins'
  echo typo >> github/chatly/lib/lib.txt
  run "$page" 'git -C github/chatly/lib commit -am "fix the lib typo"'
  expect "the fix is published under its own name" git -C "$SANDBOX/remotes/chatly/lib.git" rev-parse --verify -q lib-fix
  sub "$page" 'cd ../../<weave>' 'cd ../../work'
  step "$page" 'rwv workweave delete pr-42 --discard-unlanded'
  expect "the review workweave is gone" test ! -d "$HOME/.workweaves/web-app--pr-42"
}

# A landing that stops in its retire: a submodule store of the workweave holds a branch no remote has.
retire_stop() { # retire_stop <workweave name>
  rwv workweave create "$1" >/dev/null 2>&1 || { fail "create $1"; return 1; }
  cd "$HOME/.workweaves/web-app--$1" || exit 2
  commit_in github/chatly/server "$1.txt" "server: $1"
  git -C github/chatly/server/vendor switch -qc keep && git -C github/chatly/server/vendor commit -q --allow-empty -m "vendor: local only" \
    && git -C github/chatly/server/vendor switch -q --detach main || fail "unpublished work in the submodule store"
  stops "the retire meets a store holding unpublished work" rwv land --retire
  grep -qF 'rwv explain dropped-repo-has-unique-commits' <<<"$OUT" || fail "claim not met: the retire refuses for the store (got: $OUT)"
}

recipe_stops() {
  local resume=$HOWTO/resume-or-abort-mid-op-sync.md ww
  sandbox
  remote chatly/vendor
  run "$TUTORIAL" 'mkdir ~/work && cd ~/work'
  ww=$HOME/.workweaves
  rwv branch set-history linear >/dev/null 2>&1 || fail "history"
  git -C github/chatly/server submodule add -q https://github.com/chatly/vendor.git vendor 2>/dev/null \
    && git -C github/chatly/server commit -qm "server: vendor submodule" && rwv lock --commit >/dev/null 2>&1 && rwv push >/dev/null 2>&1 || fail "submodule member"

  retire_stop w1
  expect "the landing itself is done" test -f "$HOME/work/github/chatly/server/w1.txt"
  says "status opens with the operation" "land in progress" rwv status
  echo "+ rwv land --continue --discard-unpublished   (printed)"
  grep -qF '`rwv land --continue --discard-unpublished`' <<<"$OUT" || fail "not printed: rwv land --continue --discard-unpublished"
  rwv land --continue --discard-unpublished || fail "exit $?: rwv land --continue --discard-unpublished"
  expect "the retire went through" test ! -d "$ww/web-app--w1"

  cd ~/work && retire_stop w2
  (cd ~/work && rwv workweave create w3 >/dev/null 2>&1 && commit_in "$ww/web-app--w3/github/chatly/web" w3.txt "web: w3" \
    && cd "$ww/web-app--w3" && rwv land --retire >/dev/null 2>&1) || fail "a second landing builds on the target"
  cd "$ww/web-app--w2" || exit 2
  stops "abort over a target built on" rwv abort
  grep -qF 'rwv explain foreign-tip' <<<"$OUT" || fail "claim not met: abort refuses whole as foreign-tip (got: $OUT)"
  run_printed 'rwv abort --leave-target'
  stops "the operation has ended" rwv abort
  grep -qF 'rwv explain no-op-recorded' <<<"$OUT" || fail "claim not met: no operation is left"
  expect "the target is left as it stands" test -f "$HOME/work/github/chatly/server/w2.txt" -a -f "$HOME/work/github/chatly/web/w3.txt"
  rwv land --continue >/dev/null 2>&1; rwv workweave delete w2 --discard-unpublished >/dev/null 2>&1

  cd ~/work && rwv workweave create a >/dev/null 2>&1 && rwv workweave create b >/dev/null 2>&1 || fail "create a, b"
  commit_in "$ww/web-app--a/github/chatly/web" shared.txt "web: a"; commit_in "$ww/web-app--b/github/chatly/web" shared.txt "web: b"
  (cd "$ww/web-app--a" && rwv land --retire >/dev/null 2>&1) || fail "land a"
  cd "$ww/web-app--b" || exit 2
  stops "the second landing stops on the conflict" rwv land
  sed -i 's/"schema": [0-9]*/"schema": 999/' .rwv-op
  stops "a record rwv cannot read refuses abort" rwv abort
  grep -qF 'rwv explain unreadable-op-record' <<<"$OUT" || fail "claim not met: abort refuses as unreadable-op-record (got: $OUT)"
  expect "status still runs and reports it" rwv status
  sub "$resume" 'rwv -C <workspace> abort --discard-unreadable-record' "$(printed 'abort --discard-unreadable-record')"
  expect "the record is gone" test ! -e .rwv-op
  expect "nothing was moved back" test -d github/chatly/web/.git -o -f github/chatly/web/.git
  git -C github/chatly/web rebase --abort
  (cd ~/work && rwv doctor --fix >/dev/null 2>&1; rwv doctor >/dev/null 2>&1) || fail "doctor --fix settles the savepoints the record left"
}

recipe_lineage() {
  local p1 ww
  sandbox
  run "$TUTORIAL" 'mkdir ~/work && cd ~/work'
  ww=$HOME/.workweaves
  rwv branch set-history linear >/dev/null 2>&1 || fail "history"
  p1=$(git -C github/chatly/server rev-parse HEAD)
  commit_in github/chatly/server two.txt "server: two" && rwv lock --commit >/dev/null 2>&1 || fail "second pin"
  rwv workweave create f >/dev/null 2>&1 || fail "create f"
  commit_in "$ww/web-app--f/github/chatly/server" f.txt "server: f"
  rwv member pin github/chatly/server "$p1" --commit >/dev/null 2>&1 && git -C github/chatly/server reset -q --hard "$p1" || fail "the line leaves the pin"
  commit_in github/chatly/server q.txt "server: q" && rwv lock --commit >/dev/null 2>&1 || fail "the line moves on"
  cd "$ww/web-app--f" || exit 2
  stops "work built on a line the target left" rwv land
  grep -qF 'pin off lineage' <<<"$OUT" || fail "claim not met: the landing refuses as pin off lineage (got: $OUT)"
  expect "nothing moved" test "$(git -C "$HOME/work/github/chatly/server" log -1 --format=%s)" = "server: q"
  run_printed 'rwv sync --reset-to-target github/chatly/server'
  says "the reset kept the work as a backup" "server: f" rwv backup list
  expect "the feature lands now" rwv land --retire
}

for p in $PAGES; do
  echo "== $p"
  cd / || exit 2
  "recipe_${p//-/_}"
done

echo "pending: $PENDING"
if [ "$FAILS" -ne 0 ]; then echo "FAILED: $FAILS" >&2; exit 1; fi
echo "OK"
