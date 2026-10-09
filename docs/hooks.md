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

### Run 30 (user): writers found

The save block **is** written during play. Run 29's empty log is unexplained; maybe the save was found after the actions.
- **The copy search** found nothing to add (it is now off unless `MH3U_WATCH_COPY` is set).
- **The overlay already re-parses this block every frame**, so its counts are live.
- **Money** is the word at `+0x48` (the labels are fixed; the write at `+0x4c`, 0x7b6b, comes before any action and is still
  unknown).
- **Not seen:** no write reached the equipment box, so crafting or upgrading armour isn't found. Neither is quest start/end as
  such.
- **Host copies:** two runtime copies (`[watch-host]`) wrote the 96-byte pouch, one early and one at the end, probably the
  supplies after the quest.

Hook-point table. The actions are matched by the order of the lines and the values written, so they are guesses until the
confirm run:

| Action | Function (`mods/hooks.txt` name) | Evidence | Confidence |
|---|---|---|---|
| use / drop a pouch item | `f_0219ae84` `pouch_clear_slot`, `f_0219acec` `pouch_set_slot` | pouch 4 count 0, id 0 | medium |
| item into the pouch (pickup, or from the box) | `f_0219acec` `pouch_set_slot` | pouch 4 <- id 0x18, count 1 | medium |
| move box -> pouch / materials used | `f_02198770` `box_take` | box 124 count 0x13, box 126 cleared | medium |
| item into the box | `f_02199438` `box_put`, `f_02199408` `box_set_count` | box 126 <- id 0x45e, count 1; box 35 count 12 | low–medium |
| buy / sell, smithy fee | `f_0219c720` `money_set` | +0x48: 24015 -> 23815 (lr 0x2217cd4, 0x22087d4) | high (that it writes money) |
| quest start / return | `f_0216afd4` `money_set_quest`, `f_02198d40` `loadout_write` | money 23505 then back to 23815 at the end; +0x50..+0xbf rewritten twice | low |
| craft / upgrade equipment | not found | no equipment-box write | – |

From that log, the functions that wrote each field go into `mods/hooks.txt`. Menu-open and quest hooks need a second pass:
`MH3U_ENTER_LR` on the callers found here, or watching a menu or quest field once one is known.

### Run 31 (user): confirm run

The user logged each action with the hook lines it produced (`MH3U_HOOK_LOG=1`). In every hook, `r3 = 0x2e686af0`: the hunter's
data, 0x10 before the save block (except `0x0216afd4`).

| Hook | Function | Verdict | Arguments | Seen on |
|---|---|---|---|---|
| `money_add` | `f_0219c720` | **confirmed** | r4 = signed zenny change, r5 = &money (+0x48) | craft fees -200/-300, meal -50, quest fee -310, refund +310 on abandon |
| `item_gain` | `f_02199438` | likely (20 calls in all) | r5 = item id (0xb4 on a pickup), r4 = &save | pickup; after box -> pouch |
| `pouch_remove_slot` | `f_0219ae84` | likely (1 call) | r4 = r5 = the pouch | putting a pouch item into the box |
| `equip_records` | `f_02198d40` | likely, for crafting armour | r5 = record kind 0x10..0x19 | crafting the Piscine Mask; earlier around the quest |
| (was `pouch_set_slot`) | `f_0219acec` | rejected: a pouch item lookup, thousands of calls | r4 = item id, r5 = the pouch | everywhere |
| (was `box_set_count`) | `f_02199408` | rejected: a box lookup | r4 = item id | everywhere |
| (was `box_take`) | `f_02198770` | rejected: a field getter, ~200k calls | r5 = field 7/8 | every frame |
| (was `money_set_quest`) | `f_0216afd4` | rejected: per frame, another object (r3 0x2e66af50) | – | every frame |

Still missing, with the cheapest next step for each:
- **Craft (decorations, weapons):** the craft's money change (`money_add` from lr `0x22087d4`, or `0x220715c` for armour) is
  inside the craft code. Hook the functions containing those lrs (look them up in `game.h`: the nearest `f_` at or below the
  address) and confirm.
- **Quest start / end:** the same way: the callers of `money_add`, at lr `0x21e3724` (quest fee) and `0x2172ca4` (abandon
  refund). For a finished quest, the reward is likely another `money_add` caller.
- **Item used:** use an item with `MH3U_HOOK_LOG=1` and watch `item_gain` and `pouch_remove_slot`. If neither fires, run the
  live watch again on that pouch slot.
- **Menu open:** no data field is known. `MH3U_PROFILE` with the menu open versus closed would show the menu's functions.

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

- Status: step 5 is done for money and item gain (run 31). Four hooks are kept, four are rejected (PR #14, ready for review).
- Next: craft and quest hooks through the callers of `money_add` (the lrs above); then step 6 (exit hooks), or step 7 (a hook
  that drives the overlay: refresh the wishlist on `item_gain` and `money_add`).
