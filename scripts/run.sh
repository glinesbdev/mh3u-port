#!/usr/bin/env bash
# Runs the game in a window on Linux, writing what it prints to a log as well: the same as scripts/win-run.ps1.
#   scripts/run.sh                         # play; output also in ./mh3u_latest.log
#   scripts/run.sh --fps                   # with FPS and the [draw times] lines
#   scripts/run.sh --dump frames --frames 2400,3000
# Build first: cargo build --release -p mh3u-tools, then target/release/mh3u-tools build-game OUT --native. See docs/translator.md.
set -euo pipefail

usage() {
    cat <<'USAGE'
usage: scripts/run.sh [options]
  --games DIR        the folder with the [Game] and [Update] dumps ($MH3U_GAMES, else ~/Downloads/wiiu)
  --out DIR          the translated C and libgame.so ($MH3U_OUT, else ~/.local/share/mh3u/c)
  --fullscreen       borderless fullscreen on the window's monitor (F11 or Alt+Enter toggles it while playing)
  --fps              print the FPS and the graphics timings (MH3U_FPS, MH3U_GFX_TIMES)
  --report           print the graphics report at the end (MH3U_GFX_REPORT)
  --dump DIR         write frames to DIR (MH3U_GFX_DUMP_DIR); which ones is --frames
  --frames N[,N...]  the frames --dump writes, counting swaps from 0 (default 2400)
  --debug-ps MODE    replace what pixel shaders put out (MH3U_GFX_DEBUG_PS): white, alpha1, tex, uv, inputN, ...
  --pad-layout L     xbox (the default) or nintendo (MH3U_PAD_LAYOUT)
  --log FILE         the log (default ./mh3u_latest.log; an older one is renamed after the time it was written)
USAGE
}

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
games="${MH3U_GAMES:-$HOME/Downloads/wiiu}"
out="${MH3U_OUT:-$HOME/.local/share/mh3u/c}"
fullscreen=0 fps=0 report=0 dump="" frames="2400" debug_ps="" pad_layout="" log=""
while [ $# -gt 0 ]; do
    case "$1" in
        --games) games="$2"; shift 2 ;;
        --out) out="$2"; shift 2 ;;
        --fullscreen) fullscreen=1; shift ;;
        --fps) fps=1; shift ;;
        --report) report=1; shift ;;
        --dump) dump="$2"; shift 2 ;;
        --frames) frames="$2"; shift 2 ;;
        --debug-ps) debug_ps="$2"; shift 2 ;;
        --pad-layout)
            case "$2" in xbox|nintendo) pad_layout="$2" ;; *) echo "--pad-layout is xbox or nintendo" >&2; exit 2 ;; esac
            shift 2 ;;
        --log) log="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

tools="$repo/target/release/mh3u-tools"
for p in "$tools" "$out/libgame.so" "$games"; do
    if [ ! -e "$p" ]; then
        echo "not found: $p (build first, or pass --games/--out)" >&2
        exit 1
    fi
done

# debugging variables left over in this shell would change what the game draws or prints
for name in MH3U_FPS MH3U_GFX_TIMES MH3U_GFX_REPORT MH3U_GFX_LOG MH3U_GFX_DUMP_FRAME MH3U_GFX_DUMP_DIR \
    MH3U_GFX_DEBUG_PS MH3U_GFX_DEBUG_PS_ONLY MH3U_GFX_SOLID MH3U_GFX_DEPTH_BIAS MH3U_GFX_TEXDUMP \
    MH3U_GFX_SAVE MH3U_GFX_SAVE_EVERY MH3U_GFX_SAVE_IMAGE MH3U_PAD_LAYOUT; do
    if [ -n "${!name+x}" ]; then
        echo "cleared $name (was '${!name}')"
        unset "$name"
    fi
done
if [ "$fps" = 1 ]; then export MH3U_FPS=1 MH3U_GFX_TIMES=1; fi
if [ "$report" = 1 ]; then export MH3U_GFX_REPORT=1; fi
if [ -n "$dump" ]; then export MH3U_GFX_DUMP_DIR="$dump" MH3U_GFX_DUMP_FRAME="$frames"; fi
if [ -n "$debug_ps" ]; then export MH3U_GFX_DEBUG_PS="$debug_ps"; fi
if [ -n "$pad_layout" ]; then export MH3U_PAD_LAYOUT="$pad_layout"; fi
env | grep '^MH3U_' | sort || true

if [ -z "$log" ]; then
    log="$PWD/mh3u_latest.log"
    if [ -e "$log" ]; then
        kept="mh3u_$(date -r "$log" +%Y-%m-%d_%H%M%S).log"
        mv -- "$log" "$PWD/$kept"
        echo "kept the previous log as $kept"
    fi
fi
extra=()
[ "$fullscreen" = 1 ] && extra+=(--fullscreen)
echo "> $tools run $games $out --window --max-imports 1000000000000 ${extra[*]}"
echo "log: $log"
code=0
"$tools" run "$games" "$out" --window --max-imports 1000000000000 "${extra[@]}" 2>&1 | tee -- "$log" || code=$?
echo "exit code $code; log: $log"
exit "$code"
