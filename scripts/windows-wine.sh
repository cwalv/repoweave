#!/usr/bin/env bash
# Runs the test suite as x86_64-pc-windows-gnu binaries under Wine: a local
# lead on Windows-only failures, ahead of a .github/workflows/windows-tests.yml
# run.
#
# Usage: scripts/windows-wine.sh [cargo test target selectors] [-- test filter]
#
# With no selectors it runs every test target, which is what
# `scripts/ci-local.sh --stages=windows-wine` invokes. Selectors pass through
# to cargo (`--test add_remove_test`, `--lib`), and anything after `--` passes
# to every selected test binary. A whole-suite run takes about as long as that
# workflow's job; a run of the binaries a Windows red names takes minutes.
#
# Wine is not Windows, so a red here is a lead to confirm there, and a green
# here is not a green there. A test that fails here for a reason of this
# environment's and not the code's is listed in KNOWN_REDS below, with that
# reason and its result on real Windows, and its failure is reported as known.
# Any other failure fails the run, and so does an entry whose test passes here
# or did not run where the run should have reached it.
#
# Needs nix, rustup, curl, unzip, zstd, jq, perl and the x86_64-pc-windows-gnu
# target. The first run downloads Wine, a mingw-w64 linker, MinGit, a Windows
# build of binutils' dlltool and assembler, and a Windows-hosted Rust toolchain
# into <target-dir>/windows-wine, about 5 GB with the prefixes.

set -euo pipefail

NIXPKGS=github:NixOS/nixpkgs/f20cd90e28fe0403873f473b29e42662cd86e8c4
MINGIT_TAG=v2.56.0.windows.2
MINGIT_ZIP=MinGit-2.56.0.2-64-bit.zip
MINGIT_SHA256=da35e72aa21c005a5a0d298cfbae110bc1609a815730ea0dde84b01a1b3cd3be
MSYS2_REPO=https://repo.msys2.org/mingw/mingw64
MSYS2_PACKAGES=(
    "mingw-w64-x86_64-binutils-2.47-4-any.pkg.tar.zst e36c52a7fe1d104c2efd97dc74162386eabf8323f6aeb9b83aeb574b291a7da0 mingw64/bin/as.exe mingw64/bin/dlltool.exe"
    "mingw-w64-x86_64-gettext-runtime-1.0-1-any.pkg.tar.zst be68d7f260633284b910c588c6d82ee304a81c8817a686d2cd9df83f872c27af mingw64/bin/libintl-8.dll"
    "mingw-w64-x86_64-libiconv-1.19-1-any.pkg.tar.zst 21e334d0911f25de75d3e18e0697648bcecfa9658256d600cad0827d719c2f35 mingw64/bin/libiconv-2.dll"
    "mingw-w64-x86_64-zlib-1.3.2-2-any.pkg.tar.zst 9e75842a070ba648e986e12424e1c92c9d7d77200e85f6a34eeb600819f2e694 mingw64/bin/zlib1.dll"
    "mingw-w64-x86_64-zstd-1.5.7-2-any.pkg.tar.zst 1add6705b344664f6aca108c85f79ab5bdd9e1162662bb06a4cf40a34f6e0907 mingw64/bin/libzstd.dll"
)
TARGET=x86_64-pc-windows-gnu
jobs="${WINE_JOBS:-4}"

# One `<target> <test>` line per test. The indented `windows:` and `reason:`
# lines after a run of them hold for every test in that run.
KNOWN_REDS='
case_equivalence_test a_confusable_sibling_warns_at_mint_and_is_still_created
case_equivalence_test workweave_reuse_refuses_to_adopt_a_case_twin_directory
    windows: passed on windows-tests.yml at b847d11
    reason: Wine folds case on lookup, so the test takes its folding arm, but
        Wine canonicalizes a path to the spelling it was asked for where NTFS
        gives the spelling on disk. rwv matches a path to the listing of its
        parent by comparing canonical paths, so it never finds an entry listed
        under another spelling. A case-folding directory gets the same answer.
case_equivalence_test every_arm_this_host_cannot_reach_announces_itself
    windows: passed on windows-tests.yml at b847d11
    reason: it reruns workweave_reuse_refuses_to_adopt_a_case_twin_directory
        and requires it to pass.
case_equivalence_test case_drift_between_the_record_and_the_disk_is_reported
    windows: passed on windows-tests.yml at b847d11
    reason: Wine reports a rename that changes only case as done and leaves
        the name on disk as it was, so the drift the test makes never happens.
e2e_ecosystem_constraints_test npm_workspace_checks_version_mismatch
e2e_ecosystem_constraints_test go_work_catches_version_mismatch
e2e_ecosystem_constraints_test uv_workspace_silently_allows_version_mismatch
    windows: passed on windows-tests.yml at b847d11
    reason: it measures npm, go or uv itself and fails rather than skips
        where that tool is missing. None of them is installed under Wine,
        where each would measure only the tool and nothing of rwv.
'

cargo_args=()
test_args=()
while [ "$#" -gt 0 ]; do
    if [ "$1" = "--" ]; then
        shift
        test_args=("$@")
        break
    fi
    cargo_args+=("$1")
    shift
done
whole_suite=false
if [ "${#cargo_args[@]}" -eq 0 ]; then
    cargo_args=(--lib --bins --tests)
    whole_suite=true
fi

known_reds=$(printf '%s\n' "$KNOWN_REDS" | awk '
    function complain(what) {
        printf "windows-wine: KNOWN_REDS %s\n", what > "/dev/stderr"
        bad = 1
    }
    function flush(   i, first) {
        first = names[1]
        sub(/\t/, " ", first)
        if (windows == "" || reason == "")
            complain(first " has no " (windows == "" ? "windows:" : "reason:") " line")
        for (i = 1; i <= n; i++) printf "%s\t%s\t%s\n", names[i], windows, reason
        n = 0; windows = ""; reason = ""; field = ""
    }
    /^[[:space:]]*$/ { next }
    /^[^[:space:]]/ {
        if (field != "") flush()
        if (NF != 2) complain("line is not `<target> <test>`: " $0)
        names[++n] = $1 "\t" $2
        next
    }
    { sub(/^[[:space:]]+/, "") }
    n == 0 { complain("line follows no test: " $0); next }
    sub(/^windows:[[:space:]]*/, "") { field = "windows"; windows = $0; next }
    sub(/^reason:[[:space:]]*/, "") { field = "reason"; reason = $0; next }
    field == "windows" { windows = windows " " $0; next }
    field == "reason" { reason = reason " " $0; next }
    { complain("line is neither windows: nor reason: " $0) }
    END { if (n) flush(); exit bad }
')

if ! rustup target list --installed | grep -qx "$TARGET"; then
    printf 'windows-wine: %s is not installed — rustup target add %s\n' "$TARGET" "$TARGET" >&2
    exit 1
fi
missing=()
for tool in nix curl unzip zstd jq perl sha256sum; do
    command -v "$tool" >/dev/null 2>&1 || missing+=("$tool")
done
if [ "${#missing[@]}" -gt 0 ]; then
    printf 'windows-wine: needs %s on PATH\n' "${missing[*]}" >&2
    exit 1
fi

package_dir=$(pwd)
target_dir=$(cargo metadata --format-version 1 --no-deps | jq -r .target_directory)
cache="$target_dir/windows-wine"
mkdir -p "$cache"

nix_out() {
    nix build --no-link --print-out-paths "$NIXPKGS#$1^out"
}
# Wine 11.0 creates a relative symlink whose target climbs with `..` but
# cannot open a path through it, which is the shape of every weave-root link.
# 11.19 can; the pinned nixpkgs carries it as wineWow64Packages.unstable.
wine_bin="$(nix_out wineWow64Packages.unstable)/bin"
mingw_bin="$(nix_out pkgsCross.mingwW64.buildPackages.gcc)/bin"
pthreads_lib="$(nix_out pkgsCross.mingwW64.windows.pthreads)/lib"
mingw_lib="$(nix_out pkgsCross.mingwW64.windows.mingw_w64)/lib"
export PATH="$wine_bin:$mingw_bin:$PATH" WINEDEBUG=-all

mingit="$cache/mingit"
if [ ! -f "$mingit/cmd/git.exe" ]; then
    curl -fsSL -o "$cache/$MINGIT_ZIP" \
        "https://github.com/git-for-windows/git/releases/download/$MINGIT_TAG/$MINGIT_ZIP"
    printf '%s  %s\n' "$MINGIT_SHA256" "$cache/$MINGIT_ZIP" | sha256sum -c --quiet
    rm -rf "$mingit.partial"
    unzip -q -d "$mingit.partial" "$cache/$MINGIT_ZIP"
    # The msys runtime prints "Cygwin WARNING: Couldn't compute FAST_CWD
    # pointer" to stderr on every start under Wine, with no setting to stop
    # it, and git runs each local-path clone and fetch through that runtime's
    # sh. Emptying the format string silences that one warning.
    dll="$mingit.partial/usr/bin/msys-2.0.dll"
    if ! perl -0777 -pi -e '$n += s/C(ygwin WARNING:\n  Couldn\x27t compute FAST_CWD)/\x00$1/; END { exit($n == 1 ? 0 : 1) }' "$dll"; then
        printf 'windows-wine: the FAST_CWD warning is not in %s\n' "$dll" >&2
        exit 1
    fi
    cp "$mingit.partial/etc/fstab" "$mingit.partial/etc/fstab.dist"
    rm -rf "$mingit"
    mv "$mingit.partial" "$mingit"
fi

# rustc builds a raw-dylib import library for windows-gnu with binutils'
# dlltool, which runs the assembler in its own directory. The Windows
# toolchain's dlltool has none there, and Wine cannot wait on a host process,
# so the Windows toolchain under Wine gets a Windows build of both.
binutils="$cache/binutils"
if [ ! -f "$binutils/dlltool.exe" ]; then
    rm -rf "$binutils.partial"
    mkdir -p "$binutils.partial"
    for package in "${MSYS2_PACKAGES[@]}"; do
        read -r file sha256 members <<< "$package"
        read -ra members <<< "$members"
        curl -fsSL -o "$cache/$file" "$MSYS2_REPO/$file"
        printf '%s  %s\n' "$sha256" "$cache/$file" | sha256sum -c --quiet
        zstd -dcq "$cache/$file" \
            | tar -x -C "$binutils.partial" --strip-components=2 "${members[@]}"
    done
    rm -rf "$binutils"
    mv "$binutils.partial" "$binutils"
fi

channel=$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
windows_toolchain="$cache/rustup/toolchains/$channel-$TARGET"
if [ ! -f "$windows_toolchain/bin/cargo.exe" ]; then
    RUSTUP_HOME="$cache/rustup" rustup toolchain install "$channel-$TARGET" \
        --profile minimal --force-non-host --no-self-update
fi

# env!("CARGO_MANIFEST_DIR") and env!("CARGO_BIN_EXE_rwv") are host paths
# such as /home/me/repoweave. A Windows process reads one as rooted on the
# current drive, and msys reads it as rooted at its own install, so each gets
# the host directory mounted at that spelling.
roots=()
for dir in "$package_dir" "$target_dir"; do
    top="/$(printf '%s' "$dir" | cut -d/ -f2)"
    [[ " ${roots[*]} " == *" $top "* ]] || roots+=("$top")
done
{
    cat "$mingit/etc/fstab.dist"
    for top in "${roots[@]}"; do
        printf 'Z:%s %s ntfs binary,posix=0,noacl,user 0 0\n' "$top" "$top"
    done
} > "$mingit/etc/fstab"

for ((slot = 0; slot < jobs; slot++)); do
    prefix="$cache/prefix-$slot"
    if [ ! -d "$prefix/drive_c" ]; then
        if [ -d "$cache/prefix-0/drive_c" ]; then
            cp -a "$cache/prefix-0" "$prefix"
        else
            WINEPREFIX="$prefix" wineboot -i >/dev/null 2>&1
            WINEPREFIX="$prefix" wineserver -w
            printf '[user]\n\temail = ci@example.com\n\tname = CI\n' \
                > "$prefix/drive_c/users/$(id -un)/.gitconfig"
        fi
    fi
    for top in "${roots[@]}"; do
        link="$prefix/drive_c/${top#/}"
        [ -L "$link" ] && continue
        if [ -e "$link" ]; then
            printf 'windows-wine: %s already exists in the prefix; cannot mount %s there\n' "$link" "$top" >&2
            exit 1
        fi
        ln -s "$top" "$link"
    done
done

winpath() {
    printf 'Z:%s' "${1//\//\\}"
}

run_dir="$cache/run"
rm -rf "$run_dir"
mkdir -p "$run_dir"

run_wine() {
    local slot=$1
    shift
    # The host toolchain's variables would reach the Windows process as they
    # are: CARGO_HOME would point the in-Wine cargo at the host's registry,
    # and a weave's rustc-wrapper names a program Wine cannot start. Wine
    # encodes a non-ASCII file name for the host with the host locale, which
    # a Nix-built Wine does not find under the caller's LANG and falls back
    # to ASCII, so creating "café.txt" fails with NotFound. The Windows
    # toolchain's sysroot holds only the import libraries std links; gcc finds
    # the others a dependency names on LIBRARY_PATH.
    env -u CARGO_HOME -u RUSTUP_HOME -u RUSTUP_TOOLCHAIN -u RUST_RECURSION_COUNT -u LC_ALL \
        LANG=C.UTF-8 WINEPREFIX="$WINE_CACHE/prefix-$slot" WINEPATH="$WINE_WINDOWS_PATH" \
        LIBRARY_PATH="$WINE_LIBRARY_PATH" CARGO="$WINE_WINDOWS_CARGO" RUSTC_WRAPPER= "$@"
}
run_one() {
    local exe=$1
    shift
    run_wine "$SLOT" CARGO_MANIFEST_DIR="$WINE_PACKAGE_DIR" wine "$exe" "$@" \
        > "$WINE_RUN_DIR/$(basename "$exe" .exe).log" 2>&1 \
        || printf '%s\n' "$exe" >> "$WINE_RUN_DIR/failed"
}
export -f run_wine run_one
export WINE_CACHE="$cache" WINE_RUN_DIR="$run_dir" WINE_PACKAGE_DIR="$package_dir"
export WINE_WINDOWS_CARGO="$(winpath "$windows_toolchain/bin/cargo.exe")"
export WINE_WINDOWS_PATH="$(winpath "$mingit/cmd");$(winpath "$mingit/usr/bin");$(winpath "$windows_toolchain/bin");$(winpath "$binutils")"
export WINE_LIBRARY_PATH="$(winpath "$mingw_lib")"

cargo test --no-run --target "$TARGET" \
    --config "target.$TARGET.linker='x86_64-w64-mingw32-gcc'" \
    --config "target.$TARGET.rustflags=['-L','native=$pthreads_lib','-C','debuginfo=0']" \
    --message-format=json-render-diagnostics "${cargo_args[@]}" \
    | jq -r 'select(.reason == "compiler-artifact" and .executable != null and .profile.test)
             | [.target.name, .target.kind[0], .target.src_path, .executable] | @tsv' \
    > "$run_dir/built"

# A compile probe hands the rustc on PATH the rlib its own binary linked, and
# a cross-built rlib's proc-macro dependencies are host libraries no Windows
# rustc can load. A test target that uses the probe is built again by the
# Windows toolchain under Wine, which builds every dependency for Windows, and
# runs from that build.
probe_targets=()
while IFS=$'\t' read -r name kind src _; do
    if [ "$kind" = test ] && grep -qw compile_probe "$src"; then
        probe_targets+=(--test "$name")
    fi
done < "$run_dir/built"
: > "$run_dir/rebuilt"
if [ "${#probe_targets[@]}" -gt 0 ]; then
    run_wine 0 wine "$WINE_WINDOWS_CARGO" test --no-run \
        --target-dir "$(winpath "$cache/target")" \
        --config "target.$TARGET.rustflags=['-C','debuginfo=0']" \
        --message-format=json-render-diagnostics "${probe_targets[@]}" \
        | jq -r 'select(.reason == "compiler-artifact" and .executable != null and .profile.test)
                 | [.target.name, (.executable | ltrimstr("Z:") | gsub("\\\\"; "/"))] | @tsv' \
        > "$run_dir/rebuilt"
    WINEPREFIX="$cache/prefix-0" wineserver -w
fi
awk -F'\t' 'FILENAME == ARGV[1] { rebuilt[$1] = $2; next }
            { print ($1 in rebuilt) ? rebuilt[$1] : $4 }' \
    "$run_dir/rebuilt" "$run_dir/built" > "$run_dir/binaries"

for ((slot = 0; slot < jobs; slot++)); do WINEPREFIX="$cache/prefix-$slot" wineserver -p; done
xargs -a "$run_dir/binaries" -d '\n' -n 1 -P "$jobs" --process-slot-var=SLOT \
    bash -c 'run_one "${@: -1}" "${@:1:$#-1}"' _ "${test_args[@]}" || true
for ((slot = 0; slot < jobs; slot++)); do WINEPREFIX="$cache/prefix-$slot" wineserver -k || true; done

while read -r exe; do
    printf '     Running %s\n' "$exe"
    cat "$run_dir/$(basename "$exe" .exe).log"
done < "$run_dir/binaries"

touch "$run_dir/failed"
declare -A outcome=() ran=() listed=()
unfinished=()
while read -r exe; do
    name=$(basename "$exe" .exe)
    log="$run_dir/$name.log"
    target=${name%-*}
    ran[$target]=1
    exited_nonzero=false
    if grep -qxF -- "$exe" "$run_dir/failed"; then
        exited_nonzero=true
        if ! grep -q '^test result: ' "$log" || ! grep -q '^test .* \.\.\. FAILED$' "$log"; then
            unfinished+=("$target")
        fi
    fi
    # A failing test's panic message can quote a child run's result lines, and
    # a passing binary's log can carry a fixture's, so a failure is read only
    # from a binary that exited non-zero, and it outranks a pass.
    while read -r test result; do
        if [ "$result" = FAILED ]; then
            if "$exited_nonzero"; then outcome["$target $test"]=FAILED; fi
        elif [ -z "${outcome["$target $test"]:-}" ]; then
            outcome["$target $test"]=ok
        fi
    done < <(sed -n 's/^test \([^ ]*\) \.\.\. \(ok\|FAILED\)$/\1 \2/p' "$log")
done < "$run_dir/binaries"

known=()
stale=()
while IFS=$'\t' read -r target test windows reason; do
    [ -n "$target" ] || continue
    listed["$target $test"]=1
    case "${outcome["$target $test"]:-}" in
        FAILED)
            known+=("  $target $test")
            while read -r line; do
                known+=("      $line")
            done < <(printf '%s\n' "$reason" | fold -s -w 70)
            known+=("      real Windows: $windows")
            ;;
        ok) stale+=("$target $test passes under Wine") ;;
        *)
            if [ -n "${ran[$target]:-}" ] && [ "${#test_args[@]}" -eq 0 ]; then
                stale+=("$target $test did not run, though $target did")
            elif [ -z "${ran[$target]:-}" ] && "$whole_suite"; then
                stale+=("$target $test names a target the whole suite does not build")
            fi
            ;;
    esac
done <<< "$known_reds"

unlisted=()
for key in "${!outcome[@]}"; do
    if [ "${outcome[$key]}" = FAILED ] && [ -z "${listed[$key]:-}" ]; then
        unlisted+=("$key")
    fi
done
for target in "${unfinished[@]}"; do
    unlisted+=("$target exited non-zero with no failed test or no test result; see its output")
done

printf '\nwindows-wine: %s test binaries ran under Wine, %s exited non-zero\n' \
    "$(wc -l < "$run_dir/binaries")" "$(wc -l < "$run_dir/failed")"
if [ "${#known[@]}" -gt 0 ]; then
    printf '\nKnown reds, failing for the reason KNOWN_REDS gives:\n'
    printf '%s\n' "${known[@]}"
fi
if [ "${#unlisted[@]}" -gt 0 ]; then
    printf '\nFailures KNOWN_REDS does not list:\n'
    printf '%s\n' "${unlisted[@]}" | sort | sed 's/^/  /'
fi
if [ "${#stale[@]}" -gt 0 ]; then
    printf '\nStale KNOWN_REDS entries, to remove:\n'
    printf '  %s\n' "${stale[@]}"
fi
if [ "${#unlisted[@]}" -gt 0 ] || [ "${#stale[@]}" -gt 0 ]; then
    exit 1
fi
if [ "${#known[@]}" -gt 0 ]; then
    printf '\nwindows-wine: every failure under Wine is a known red\n'
else
    printf '\nwindows-wine: every test passed under Wine\n'
fi
