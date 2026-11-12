# Contributing

Thanks for helping. This page covers building, checking and sending changes.

## Never commit or post game files

This repository must never contain anything from the game. Do not commit, attach or link any of these, in pull requests, issues
or anywhere else:
- **dumps:** the base game or the update;
- **game files:** executables (`.rpx`/`.rpl`), archives, textures, models, sound, text or icons;
- **keys:** title keys or console keys;
- **saves:** save files, or wishlists from a real save;
- **anything generated from the game:** the translated C (`game.h`, `f_*.c`), Ghidra output, or `game.dll`/`libgame.so`.

`.gitignore` covers the usual names, and `translate` refuses to write inside a git working tree. Still, check `git status`
before every commit. Tests must use data made in the test, not files from a dump.

## Build and check

Set up as the [README](README.md) says, with your own dump. Before every commit, run the checks: format, clippy with warnings as
errors, the tests, the release-archive layout, `recomplauncher.json` when Python's `jsonschema` is installed, and `cargo audit`
when it is installed. GitHub Actions runs those checks on pull requests.

```sh
scripts/check.sh                                            # Linux
```

```powershell
powershell -ExecutionPolicy Bypass -File scripts\check.ps1  # Windows
```

Tests that need the game (`MH3U_GAME_DIR`) skip without it.

## Pull requests

1. Branch off `main`, one purpose for each branch: `gfx/…`, `mods/…`, `rt/…` or `docs/…`.
2. Follow [docs/code-guidelines.md](docs/code-guidelines.md). Keep `docs/`, the README and `mods/hooks.txt` in step with the code.
3. Run the checks, and test in game where you can. Say in the PR how you tested it and on which OS.
4. Anything that writes to game memory must be opt-in (an `MH3U_…` variable) and must never write saves or the pouch.
5. A new hook in `mods/hooks.txt` needs a fresh translation (`win-build.ps1 -Translate`, or `mh3u-tools translate`). Say so in
   the PR.

Report bugs with the issue form. It asks for your OS, GPU, game version, build commit, switches and the session log.

## License

The project is under the GNU AGPL-3.0 only (`LICENSE`). By contributing, you agree that your contribution is licensed the same
way. The license covers the port's own code and tools, not the game or anything made from it.
