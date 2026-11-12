# Documentation

Start with the [README](../README.md) for what the port is and how to set it up. The documents here go deeper.

## Using the port

| Document | What it covers |
|---|---|
| [windows.md](windows.md) | Building and running on Windows: toolchain, scripts and their switches, profiling |
| [../README.md](../README.md#recomplauncher) | Starting a release from RecompLauncher |
| [input.md](input.md) | Gamepads, keyboard and mouse, the bindings file |
| [overlay.md](overlay.md) | The F1 overlay: the per-hunter blacksmith wishlist, fullscreen, the software keyboard |
| [native-menu.md](native-menu.md) | The opt-in native menus (`MH3U_NATIVE_TEXT`, `MH3U_NATIVE_MENU`) and how the game's menus work |

## How it works

| Document | What it covers |
|---|---|
| [pc-port.md](pc-port.md) | The original proposal: goals, the "own the game" rule, the plan and the crates |
| [translator.md](translator.md) | The translator, the runtime and the graphics: how they work and are tested, what is not done, debugging variables |
| [audio.md](audio.md) | Sound: the AX voices, mixing, the filter and reverbs |
| [hooks.md](hooks.md) | Translation-time hooks for mods (`mods/hooks.txt`) and the events they feed |
| [performance.md](performance.md) | Measured costs and open performance work |
| [spikes.md](spikes.md) | What was measured before the port was built |

## Contributing

| Document | What it covers |
|---|---|
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | How to build, check and send changes, and what never to commit |
| [code-guidelines.md](code-guidelines.md) | Code style and the checks to run before every commit |
| [ghidra.md](ghidra.md) | Reading the game's code with Ghidra and the scripts in `scripts/ghidra/` |
| [roadmap.md](roadmap.md) | What comes next |

Several documents keep the record of how things were found, such as test runs and pull request numbers. Those parts are
history. The sections above them describe how things work now.

The addresses in these documents (`f_02153480`, `0x103d2ef4`, ...) are in the USA v32 update's executable.
