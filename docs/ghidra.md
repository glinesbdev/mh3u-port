# Ghidra: reading the game's code

For finding out what a guest function does (hooks, native menus), Ghidra's decompiler on the game's executable is quicker than
reading the translated C. Nothing here goes into the repository: the executable is the user's own dump, and the project and its
outputs stay outside any git working tree.

## Which executable

The port translates the **update's** `.rpx` when there is one (`mh3u-dump`, `Dump::executable`), so that is the one to analyse:
every `f_XXXXXXXX` address in `mods/hooks.txt` and these docs is an update address. Checked: `f_02153480` (`menu_open`),
`f_0219aeac` (`item_to_pouch`), `f_02037600` (`menu_page`) start with `mflr r0` or `stwu r1`, and `f_02199438` (`item_gain`) with
its `cmplwi r5, 0xffff`. The base game's `.rpx` is a different build: `.text` is 0xde126c bytes against the update's 0x1196384,
`.data` starts at 0x1010a000 instead of 0x10164640, and the same addresses fall in the middle of other functions there. Do not mix
the two.

## Setup (on the box, done once)

- JDK 21 (`openjdk-21-jdk-headless`), Ghidra 12.0 in `/workspace/ghidra/ghidra_12.0_PUBLIC`, and Maschell's
  [GhidraRPXLoader](https://github.com/Maschell/GhidraRPXLoader) 0.9.2 (built for Ghidra 12.0; an extension must match the
  Ghidra version exactly) unzipped into `Ghidra/Extensions/`. Its loader is named `CafeLoader`.
- The executables in `/workspace/mh3u-bin/` (mode 600, outside the repository).
- The project: imported and auto-analysed once (about 8 minutes on 8 cores):

  ```sh
  /workspace/ghidra/ghidra_12.0_PUBLIC/support/analyzeHeadless /workspace/ghidra/projects mh3u \
      -import /workspace/mh3u-bin/update.rpx -loader CafeLoader -max-cpu 8
  ```

  Ghidra's names are `FUN_02153480` for the translator's `f_02153480`; `DAT_10314578` is a global at that address.

## Scripts

`/workspace/ghidra/scripts/` holds two small Java scripts, and `/workspace/ghidra/run.sh Script.java args...` runs one read-only
on the project (`-process update.rpx -noanalysis -readOnly`) and prints only the script's output (about 5 s each):

- `Decompile.java 0x02153480 0x021546fc ...`: the C of each function containing those addresses (an `lr` from the hook log works).
- `Xrefs.java 0x02153480 ...`: every reference to each function's entry, with the function it comes from (the callers).

```sh
/workspace/ghidra/run.sh Decompile.java 0x0266298c > /workspace/ghidra/out/item-page.c
/workspace/ghidra/run.sh Xrefs.java 0x02037600
```

Vtable calls (`(**(code **)(*DAT_10314578 + 0x134))(...)`) are not resolved statically: the object is made at run time. The hook
log (`MH3U_HOOK_LOG=1`, an `in:` probe on the caller) or a watch build gives the target.
