# Keeping the code clean

Short, because the project is new. When a rule here stops being true, change it in the same commit.

1. **Nothing of the game in the repository.** No bytes, strings, tables, disassembly or generated output taken from it, and nothing that
   skips the ownership check (`docs/pc-port.md`). Counts and names of the system libraries are fine; the game's own contents are not.
2. **Files are not trusted.** A parser checks every number it reads and returns an error, never a panic or an unbounded allocation.
   Every parser has a test in `hostile_tests.rs` that feeds it damaged and made-up files.
3. **No `unsafe`** (`unsafe_code = "forbid"` for the workspace), (`mh3u-rt`, the runtime crate, is the one place that may), kept
   small and apart.
4. **Tests that need the game skip themselves without it** (`MH3U_GAME_DIR`), so the checks run on a machine with no game.
5. **Say what is known and what is guessed.** A finding goes in `docs/spikes.md` with how it was checked.
6. **Linux and Windows both.** Platform code goes behind `cfg` and a crate that is Linux only (`mh3u-cemu`) is empty elsewhere.
7. `scripts/check.sh` (on Windows PowerShell, `scripts\check.ps1`) before every commit: fmt, clippy `-D warnings`, tests, `cargo audit`. There is no CI (private repository, Actions off), so this is the only check. One purpose per commit. Windows is not built by anything automatic: build it by hand now and then (`cargo build` on a Windows machine) until a runner is worth paying for.
