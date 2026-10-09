# Translation-time hooks for mods

Mods so far only read memory (`overlay.md`). Hooks let a mod run native code when the game enters one of its functions: to know
*when* something happens (an item is added, a quest starts) and later to change what happens.

## Design

- **The list:** `mods/hooks.txt` in the repo, one hook per line (`0x02123456 name`; the address is hex and the `0x` is optional; `#` starts a comment). The translated
  functions are named `f_XXXXXXXX` in the build's `game.h` and `part_*.c` (`f_02123456` is `0x02123456`), which is where to
  look for addresses. It holds addresses
  and names only, nothing from the game. `mh3u-tools translate` reads it and copies it to the build (`<c dir>/hooks.txt`), so the
  runtime knows the names.
- **The translator** (`mh3u-recomp`) emits `rt_hook(c, 0xADDR);` at the entry of each listed function, after `ENTER` and before
  the registers are loaded into locals. So a hook sees the arguments (`r3`... `r10`, `f1`...) and the stack pointer, and may
  change them. Functions not listed get nothing, so **an unused hook costs nothing**: no call, no check.
- **The host table:** the game library's `Host` table gets one more function pointer, `hook`, appended at the end. `rt_hook`
  calls it through `host.hook`.
- **The runtime** (`mh3u-rt/src/hooks.rs`) keeps a registry of guest address -> name and callbacks. A callback gets a `Hook`
  view: the name, the address, the registers, and read/write access to guest memory through `Guest`. Registration happens before
  the game starts; at run time the registry is only read (a lock-free snapshot), so a hook that is called often stays cheap.
- **Exits (later):** at a hook's entry, the runtime will save the link register and point it at a small host trampoline, so
  the callback also runs on return (`r3` is then the result). This is not built yet.
- **Example:** the `log` callback prints the hook's name and `r3`–`r5`. It logs the first 20 calls of each hook, then every
  1000th. `MH3U_HOOK_LOG=1` puts it on every hook in the list.

## Candidate hook points

mh3u-companion reads saves and live memory, so it knows the data (`live.md`: the item box, the pouch, the equipment box, money,
HR), not the code. Hooks need the functions that change that data. Find them by watching the live save's fields change
(`MH3U_WATCH` builds report the writing function, `translator.md`):

| Hook | How to find it |
|---|---|
| item added to / taken from the pouch or box | watch an item slot's count in the live save |
| craft / upgrade at the smithy | watch the equipment box's first free slot while crafting |
| a menu opened (item box, smithy, quest board) | the function the HUD calls when it changes state; watch a menu flag |
| a quest starts / ends | watch the quest number and the clear count in the save |
| money and points paid | watch the zenny field |

## Run 28 (user, after PR #13)

Hooks work end to end. Five hooks (0x02000020, 0x02000084, 0x020000e4, 0x0200017c, 0x020001f0) were listed, translated, and
installed (`[hooks] 5 hooks installed`). Each fired with sensible arguments. For example, `0x020000e4` was called over and over
from one lr (`0x3122638`), with r3 stepping through `0x108001xx`–`0x108002xx`, which looks like a constructor walking a table.

## Finding hook points (step 5, PR #14)

`MH3U_WATCH=live`, in a watch build, logs every write the game makes to the live save once the overlay finds it. That covers
money, the worn equipment, the pouch, the item box and the equipment box (`live::WATCH_SPAN`).
- **Each line names** the field (`pouch 3 count`, `box 120 id`, `equipment 7`, `money`), the value written, the function most
  recently entered (`f_XXXXXXXX`, the writer or a function it just called), and the lr (the writer's caller, or the return
  address of its last call).
- **Repeats are cut:** for each writer (function, lr and field kind), the first 5 writes are logged, then every 100th.

### What to run (PowerShell)

```powershell
git pull; git checkout mods/hook-points
# a watch build in its own folder (the normal build in ...\mh3u\c is left alone)
.\scripts\win-build.ps1 -Watch -Out "$env:LOCALAPPDATA\mh3u\c-watch"
$env:MH3U_WATCH = 'live'
.\scripts\win-run.ps1 -Out "$env:LOCALAPPDATA\mh3u\c-watch" *> "$env:USERPROFILE\Desktop\watch.log"
Remove-Item Env:MH3U_WATCH
```

1. Load the hunter and wait for `[watch-live] watching ...` in the log (it appears when the overlay finds the live save).
2. Do each action once, with a few seconds' pause between them, and **note the order you did them in**:
   1. pick up a gathering item (pouch item added);
   2. use or drop a pouch item (removed);
   3. move an item between the pouch and the box at the item box;
   4. buy or sell something (money);
   5. craft or upgrade a piece at the smithy (equipment added, materials removed);
   6. start a quest, then come back from it (quest start and end; not watched directly, but the pouch and the supplies change).
3. Quit, and send `Desktop\watch.log` back. If it lists several copies and none is written, run again with
   `$env:MH3U_WATCH_COPY = '1'` (then `'2'`...) (attach it), with the order of the actions.

### Run 29 (user): no writes

All 6 actions were done, and the log shows the save found at `0x2e686b00` and watched (`0x2e686b49..0x2e68bad0`), but there is
not one write. All of the translated code's stores go through the `WATCH` check, so the block was not written at all. It is the
**save image**: the copy the game loads and writes when saving. That is why the overlay's counts were right in run 27. The pouch
and box the game changes during play are kept somewhere else.

Fix (`mods::watch_copies`): once the save is found, a background thread reads its pouch (24 slots) and searches guest memory
for other copies of those 96 bytes. It logs them all (`[watch-live] the pouch is also at ...`) and moves the watch to copy
`MH3U_WATCH_COPY` (0, the first, by default). Writes are then labelled `copy 0x…: pouch 3 count`, using the save's layout
around the copy; where the layout differs, labels read `pouch -N` or `save +0x…`. The pouch needs at least 3 items for the
search.

Hook-point table (filled in from the next log):

| Action | Function | Confidence |
|---|---|---|
| item added (pickup) | not found yet | – |
| item used / removed | not found yet | – |
| pouch <-> box | not found yet | – |
| money (buy/sell) | not found yet | – |
| craft / upgrade | not found yet | – |
| quest start / end | not found yet (second pass) | – |

From that log, the functions that wrote each field go into `mods/hooks.txt`. Menu-open and quest hooks need a second pass:
`MH3U_ENTER_LR` on the callers found here, or watching a menu or quest field once one is known.

## Plan

1. Write this doc. **(done)**
2. Translator: the hooks list parser, and `rt_hook` emitted at the listed entries; tests for both. **(done)**
3. ABI: `host.hook` in `ppc.h`, the dispatch file and `game.rs`; the list copied to the build. **(done)**
4. Runtime: `hooks.rs` (registry, `Hook` view, the `log` example, `MH3U_HOOK_LOG`); tests. **(done)**
5. Find the first real addresses (item add, quest start) with watch builds, and list them in `mods/hooks.txt`.
6. Exit hooks through a return trampoline.
7. An overlay feature that uses a hook, e.g. a "craftable now" toast fired by the item-add hook instead of polling.

## Status / next step

See the bottom of this file. It is updated with each commit of PR #13.

- Status: run 29 found no writes, because the watched block is the save image. The tool now finds and watches the working copies
  of the pouch (PR #14). `mods/hooks.txt` has no candidates yet.
- Next: the user rebuilds the watch build and repeats the 6 actions with at least 3 items in the pouch, then sends `watch.log`.
