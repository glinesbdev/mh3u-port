# Translation-time hooks for mods

Mods so far only read memory (`overlay.md`). Hooks let a mod run native code when the game enters one of its functions: to know
*when* something happens (an item is added, a quest starts) and later to change what happens.

## Design

- **The list:** `mods/hooks.txt` in the repo, one hook per line (`0x02123456 name`; `#` starts a comment). It holds addresses
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

## Plan

1. Write this doc. **(done)**
2. Translator: the hooks list parser, and `rt_hook` emitted at the listed entries; tests for both. **(done)**
3. ABI: `host.hook` in `ppc.h`, the dispatch file and `game.rs`; the list copied to the build. **(done)**
4. Runtime: `hooks.rs` (registry, `Hook` view, the `log` example, `MH3U_HOOK_LOG`); tests. **(done; milestone 1)**
5. Find the first real addresses (item add, quest start) with watch builds, and list them in `mods/hooks.txt`.
6. Exit hooks through a return trampoline.
7. An overlay feature that uses a hook, e.g. a "craftable now" toast fired by the item-add hook instead of polling.

## Status / next step

See the bottom of this file. It is updated with each commit of PR #13.

- Status: steps 1–4 are done (PR #13, the first milestone).
- Next: step 5, finding real addresses (needs a watch build on the user's machine).
