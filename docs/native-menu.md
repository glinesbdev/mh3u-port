# Native menus: how the item menu works (research)

Goal: mod entries inside the game's own menus (`roadmap.md`, "Later, possibly"). This is the map so far, from the hooks
(`hooks.md`, runs 39 to 42) and Ghidra's decompiler on the update `.rpx` (`ghidra.md`). Addresses are update addresses. Names
in quotes are guesses from what the code does.

## Opening a menu

- **`f_02154484`, "the field's menu input"**, runs every frame (`menu_probe_e` in run 41), from `f_021553c8`. `r4` is the pad
  state the menu code keeps: `+0x00` the menu's state byte, `+0x04` held buttons (bit `0x400` opens), `+0x18` and `+0x1d`, `+0x35`
  more state, `+0x32` a block mask. It works out from the game mode (`*(DAT_10314578 + 0x354)`: 6, 0x50, 0x52 to 0x57) whether a
  menu may open and which one (`uVar7`: 1, 2, 4, 5, 6).
- **`f_02153480` (`menu_open`)** is the "may the menu open now" test: the hunter (`*(r4 + 0xe30)`) is not mid-action (`+0x5e8`
  below a threshold, `+0xbae`/`+0xbaf`/`+0xbb0` clear) and `f_028673a8()` is 0. It is called once per frame, and the call with
  r5 = -1 is the one on the frame the button opens the menu, which is why that call marks an open.
- When it may open and the button is down: state byte = 2, `f_020e108c` (a sound), `f_02152b84` (copies the pad's buttons into
  the state), `f_020bfc74(.., 0xc9)` (a UI sound id), and `f_02152c48(menu, 1, kind)` for the start menu.

## The item page

- **`f_0266298c`, "the item page's update"**, runs every frame while the item list is up (`menu_probe_d`). `r3` is the page
  widget, `r4` the pouch list's state. On its first frame (`widget[0xb2] < 2`) it sets the page up and calls
  **`f_02037600(widget + 0x2d4, 3, 1)` (`menu_page`)**: the tab bar, page 3 = items. `f_02037600(tabs, page)` looks up a byte for
  the page in a 7-entry table at `0x1000b4a0`, shows or hides each tab's panes (`tabs + 0xc`, count at `tabs + 0x11e`), and
  stores the page at `tabs + 0x11c`.
- **The list state** (`r4` of `f_0266298c`): `+0x1d8` the pouch's slots (4 bytes each: `u16` item id, `u16` count), `+0x1dc` a
  second list (slots 24 and up when `+0x1a` is set), `+0x1e8`/`+0x1e9` the cursor row and column, `+0x1ec` columns per row,
  `+0xb0` the rows' layout entries (0x18 bytes each), `+0x1fc`/`+0x1fd` sub-mode flags.
- **`f_0223b018(list_mgr, state, index)`** gives the slot for an index (`DAT_10314980` is the list manager).
- **`f_026611b8`, "draw the visible rows"**: for each of up to 8 rows, **`f_02660a84(widget, row, .., slot, entry, flags)`**
  (720 calls in run 37's menu window: 8 rows a frame).

## Drawing a row (the text and font path)

`f_02660a84` fills a row's layout panes (arrays on the widget: `+0x584` name text, `+0x588` count text, `+0x594` icon,
`+0x58c` highlight):

- **Message lookup:** `(**(DAT_10314578->vtable + 0x134))(DAT_10314578, table, id)` returns a string: table 3 for UI strings
  (`0x25c` empty slot, `0x177`, `0x254` to `0x25e` the page's help lines), table 4 for item names (by item id). These are the
  game's GMD message tables (`mh3u-formats`, `.gmd`); the target function is resolved at run time (next step: an `in:` probe or
  reading `DAT_10314578`'s vtable from guest memory).
- **Setting text:** `f_02aff91c(pane, text, length)`; colour `f_02aff804(pane, &rgba)` (colours like `0xff646464` greyed,
  `0xfff0f0f0` normal); visibility `f_02af349c(pane, on)`. These look like the layout library's text-box calls (an NW4F-style
  `TextBox`), which draw with the layout's font, so mod text set this way uses the game's font.
- `f_0265da8c` / `f_0265da70` / `f_0265da5c` / `f_0265dbcc`: a small string-buffer helper (make, length, data, free) used around
  the text. The count is printed with `f_03125594(buf, 6, fmt, count)` (an `snprintf`).
- **Item data:** `f_0203a6dc(id)` gives the item's record (`+3` the pouch maximum, `+4` the icon), `f_0203a7c0(id)` its icon colour.
- **The help line:** `f_02145ed8(ui, text..)` -> `f_0213f040(ui, 2)` (child panel 2 of the UI object `DAT_10314578`) ->
  `f_021b44a0(panel, text)`.

## Input

The item page reads the cursor from its state (`+0x1e8`, `+0x1e9`); the buttons come from the pad state `f_02154484` keeps
(`+0x04` held, `+0x08`/`+0x0c`/`+0x10`/`+0x14` from `f_02152b84`: pressed, repeat and mask). The page's own button handling
(move, use, sort) is not mapped yet.

## In-game text test (`MH3U_NATIVE_TEXT=1`, PR #20)

Off by default. With `MH3U_NATIVE_TEXT=1` (`win-run.ps1 -NativeText`, `run.sh --native-text`) and a non-empty wishlist, the item
menu's last visible empty pouch slot shows `Wishlist: N of M ready` or `Wishlist: need X items` instead of its empty-slot text,
drawn by the game's own text box and font.

- **Confirmed with Ghidra:** `f_02aff91c(pane, text, length)` is a thunk to the text box's set-text method (vtable `+0xbc`).
  The row function `f_02660a84` gets message 3/`0x25c` (the empty-slot text) through the message manager, copies it into a
  string buffer (`f_0265da8c`: `strlen` and `strcpy`, so **8-bit text ending in 0**, not UTF-16), then calls
  `f_02aff91c(row's name pane, data, length)` at `0x02660e64` (returns to `0x02660e68`). The row number is in `r24` (from the
  function's `r4`), kept across the call.
- **The hook:** `text_set` on `f_02aff91c` (`mods/hooks.txt`). Its callback (`mh3u-rt/src/native_text.rs`) is registered only with
  the opt-in; without it the hook costs the registry's lookup per text set, and nothing else runs. It acts only when the return
  address is `0x02660e68`, the row is 0 to 7, the text pointer is set and the length is at most 256. Otherwise it leaves the call
  as it is.
- **Which row:** the rows are drawn in order every frame, so the last empty row of the previous frame gets the text.
- **The only write:** the text (printable ASCII, at most 63 bytes, then a 0) goes into the runtime's own buffer at `0x0ff00000`.
  That is the last MiB below `.rodata`, at the top of the runtime's own area (`cafe.rs`, `OS_AREA`, which now stops there with an
  assert). The game never allocates there. The callback only changes the call's `r4` (text) and `r5` (length). No game data is
  changed: the message, the string buffer and the pane are the game's own, untouched.
- **Log:** `[native-text] on: ...` at startup, and `[native-text] row N: "..."` each time the text written changes.
- **Risks:** the text box may keep the pointer and not copy (then it still points at the buffer, which keeps the text); a font
  without a glyph shows a box (ASCII only, to be safe); the addresses are for the update `.rpx` (`ghidra.md`).

## Next native-menu steps


Replace a string, not the layout: hook `f_02660a84` (entry: r4 = row, r6 = the slot) or the message lookup, and when the slot or
id is one chosen for the mod, return a mod string, for example the empty-slot text (`table 3, 0x25c`) on the last empty pouch slot
showing "Wishlist: 2 pieces ready". That needs:

1. Return-value hooks or a replaced call (hooks today run at entry only, `hooks.md` plan step 6), or a hook on `f_02aff91c` that
   swaps the text pointer in `r4` for a string the runtime puts in guest memory.
2. A guest buffer for mod strings (UTF-16 like the GMD text; to be checked) that the runtime owns.
3. Reading the selected slot (`+0x1e8`, `+0x1e9`) to react when the cursor is on the mod's entry, and the A button, for a first
   "mod entry" that does something.

Writing into the game's memory is a change of rule (the overlay only reads), so it would be opt-in and limited to these buffers.
