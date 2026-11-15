#!/usr/bin/env bash
# Runs the command blocks of the front-door pages against a built rwv in a
# hermetic sandbox. Usage: front-door-recipes.sh <dir holding rwv> [page...]
# Every `step` line must appear verbatim in its page; a page edit that drifts
# from this script fails here before anything runs.
set -uo pipefail

BIN=${1:?directory holding rwv}
shift || true
PAGES=${*:-tutorial readme release-line}
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
README=$ROOT/README.md
TUTORIAL=$ROOT/docs/tutorial.md
RELEASE=$ROOT/docs/how-to/maintain-a-release-line.md

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
# substitute <line>: a spelling that ships with another change runs as the spelling this build has,
# and is counted as pending until the build has the new one
substitute() {
  case "$1" in
    "rwv member set-url projects/"*)
      local rest=${1#rwv member set-url projects/} p
      p=${rest%% *}
      PENDING=$((PENDING + 1))
      REPLY="git -C projects/$p remote add origin ${rest#* }" ;;
    *) REPLY=$1 ;;
  esac
}
# run <page> <first line>: run that block's lines verbatim, in order, in this shell
run() {
  local page=$1 first=$2 nth=${3:-1} line n=0
  while IFS= read -r line; do
    n=$((n + 1))
    substitute "$line"; line=$REPLY
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
  rwv branch delete stable --target main --discard-unlanded >/dev/null 2>&1
  [ $? -eq 3 ] || fail "retire before the remote copy is gone must exit 3"
  git -C "$SANDBOX/remotes/chatly/web-app.git" branch -D stable >/dev/null 2>&1 || fail "forge delete"
  rwv fetch >/dev/null 2>&1
  run "$page" 'rwv branch delete stable --target main --discard-unlanded'
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

for p in $PAGES; do
  echo "== $p"
  "recipe_${p//-/_}"
done

echo "pending: $PENDING"
if [ "$FAILS" -ne 0 ]; then echo "FAILED: $FAILS" >&2; exit 1; fi
echo "OK"
