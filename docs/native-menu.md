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

## The start menu and a new page (research, Ghidra)

### How the start menu is built

- **Opening:** `f_02152c48(menu, open, kind)` (from the field's menu input `f_02154484`, `menu_open` above). `menu + 0x34` is
  the start menu's state (`S` below; `f_02154484`'s `r4` is the same block). It resets the state, builds the lists
  (`f_021528a0`), and then either opens the start menu screen through the UI manager, `(*ui->vtable + 0x13c)(ui, 2, 0)`, or,
  for `kind` 1 to 6, goes straight to a page (`f_02152988` and the others: `(ui, 1, 5)` and so on). Screens and pages are opened
  by number through that one virtual call (`ui` = `DAT_10314578`, made in `f_0213e5fc`).
- **Not a layout file:** the entries are filled in by code with counts written as constants. The layout only gives each row's
  panes (the shared row renderer has at most 8 rows).
  - **Tabs:** count `S+0x24` (2, or 3 when `*(DAT_103a2f90 + 0x8b4)` is set), cursor `S+0x25`, records of 0x18 bytes at
    `S+0x38` (`+1` selected, `+2` enabled).
  - **Rows of the tab:** `f_0215249c` fills them each frame. Count `S+0x20` (`menu+0x54`: 6, or 5 in another mode), cursor
    `S+0x28` (clamped), records of 0x18 bytes at `S+0xb0` (`+2` enabled, `+8` the label). There are **8 row records**
    (`S+0xb0` to `S+0x170`), so rows 6 and 7 of a 6-row tab are space the game has but does not use.
  - **Labels:** `f_021521ac(menu, list, i)` = message (table 3, `f_021520f4(list) + i`): consecutive message ids from a
    per-list base (a small switch and two byte tables at `0x10020f52` and `0x10020f6b`).
  - **Disabled rows:** each row's enabled byte is set by conditions (online, mode, the hunter's state). A disabled row is drawn
    greyed and A on it plays the "no" sound (`f_020bfc74(.., 2)`). None is permanently unused.
- **A second-level list** inside the start menu: sub-state `S+0x1f` (0 the tabs, 1 sliding in for 10 frames, 2 the list).
  Count `S+0x22`, cursor `S+0x26`, records at `S+0x170` (`+0` the action id, `+1` selected (`f_02153038`), `+2` enabled).
  A on a row stores its action id in `S+0x1e` and sets `S+0` = 1 (leave the menu to do it).

### Input and dispatch (`f_02154abc`, the start menu's update, called with `S`)

- Buttons from `S+4` (held/pressed) and `S+0xc` (repeat): `0x10` A, `0x20` B, `0x3` up/down (row cursor, wrapping:
  `f_0223a0ec`), `0xc` left/right (tab cursor: `f_0223a0e0`). `f_021548cc` handles the shortcut buttons first (`0x10000`,
  `0x80`) and can close the menu (returns 4).
- **B:** `S+0` = 3 (close), `S+1` = 0, sound `0xc9`.
- **A:** if the tab and row are enabled, a plain **if/else chain on (tab `S+0x25`, row `S+0x28`)**: tab 0 rows 0 to 5, tab 1
  rows 0 to 5, tab 2 rows 0 to 4. Each calls the row's opener (e.g. tab 0 row 0: `f_02152988`, which opens UI (1, 5); row 5:
  `f_0215481c` or `f_02154808`). It is not a jump table or function pointers, so a new row needs a hook here, not a table entry.
- `S+0`: 0 idle, 1 doing an action, 2 open, 3 closing.

### Adding an entry: add, not hijack

Adding is feasible without patching code, with entry hooks that only touch the menu's own state:

1. **Count and record:** after `f_0215249c` fills tab 0 (a hook on its caller, or on `f_02152874`, which runs right after it),
   set `S+0x20` from 6 to 7 and fill record 6 (`S+0xb0 + 6*0x18`: enabled, label). That record is unused space inside the
   game's own state block. This is a change to game memory, so opt-in as in PR #20.
2. **Label:** the row's text is set through `f_02aff91c` like any other; the `text_set` hook from PR #20, keyed on the start
   menu's row-draw return address and row 6, gives it "Mods".
3. **Action:** a hook on `f_02154abc` entry: when A is pressed with tab 0, row 6, clear A in `S+4` for that call (so the game's
   chain does nothing, as for a row past its last case) and open the mod page.

Hijacking an existing row would need the same three pieces and would take a game feature away, so adding is the better choice.

### The page to reuse

The start menu's **own second-level list** (`S+0x1f` = 2): text rows, a cursor, count `S+0x22`, an enabled flag per row, A and B
already handled, drawn by the start menu's layout (same font, no new layout). To open it with mod rows: set `S+0x22`, fill the
records at `S+0x170` with action ids the game does not use, set `S+0x1f` = 1 (it slides in), and swap the rows' labels
through `text_set`. A on a mod row puts its action id in `S+0x1e` and sets `S+0` = 1; a hook catches an action id of its own
there before the game acts on it. The item page (`f_0266298c`) is a grid with icons and counts, so it fits a mod list less well.

### The probe (`MH3U_MENU_PROBE=1`, PR 1)

Opt-in and read-only (`win-run.ps1 -MenuProbe`, `run.sh --menu-probe`; `mh3u-rt/src/menu_probe.rs`). Two hooks:
`menu_start` on `f_02152c48` logs `[menu-probe] open (kind K, S 0x…); before the reset: …` (S as it was before the game resets
it, so a later run can tell whether edits from the last open are still there), and `menu_update` on `f_02154abc` logs a line
each time S changes: `state`, `sub`, `action`, the tabs (count, cursor, enabled bytes), the rows (count, cursor, and per row
the enabled byte, the label pointer at `+8` and its text) and the second-level list (count, cursor, and per record the action
id and enabled byte). The line where the state becomes 3 ends in `(closing)`. Only labels pointing into `0x10000000`–`0xf0000000`
are read, at most 40 bytes, printable ASCII.

### Run 43 (the probe, build `298f9eb`): what it confirmed

The log covers one open of the start menu with the cursor moved over Hunter tab rows 0 to 3, Item Pouch and Combine entered
and left. No tab switch, second-level list, close or second open is in it.

- **Confirmed:** `S` = `menu + 0x34` (`0x2e65df94`; `menu_update`'s `r4`). Tabs `S+0x24` = 2 ("Hunter", "Info"; both enabled),
  cursor `S+0x25` = 0. Rows `S+0x20` = 6, cursor `S+0x28` follows up and down. Row records at `S+0xb0`, `+2` enabled
  (all 1), `+8` the label: a pointer to the 8-bit text (`0x18fe7160` and on), which reads, in order, **Item Pouch, Combine,
  Combo List, Hunter's Notes, Gestures, Quit Game**, as on screen. The open logged zeros before the reset (the first open).
- **`S+0x1e`** became 1 after Item Pouch and 2 after Combine: the last page entered, not only the second list's action id.
- **`S+0` stayed 0** in every logged frame (the menu's states 1 and 3 do not show while `f_02154abc` is running).
- **Hunter's Notes is not the second-level list:** `S+0x1f` stayed 0. It opens its own page (`f_022365a8`, Monster List /
  Hunter Info). The second-level list (`S+0x1f` = 2, labels from list 0x13) belongs to **Quit Game** (Hunter row 5), and on the
  Info tab to row 3 (list 0x14) (`f_026615a8`).
- **The start menu's row drawing (`f_026615a8`) has 6 row panes:** it draws rows while `i < S+0x20` and stops at 6 (`if (5 < i)`;
  the pane arrays at `widget[0x161]` to `widget[0x164]` have 6 entries). A 7th row would never be drawn, while the cursor would
  still move to it (the wrap uses the count). **So a "Mods" row cannot be added by raising the count to 7**, against the plan
  above: that needs the layout's panes, or code, changed.
- The row text is set with `text_set` from `0x026625a0` (the label pointer and its `strlen`).

### Revised plan

1. **Hijack a row label and action with a held modifier, opt-in:** with `MH3U_NATIVE_MENU=1`, while a chosen button is held (or
   after toggling with a key), Gestures' label reads "Mods" (its record's `+8` pointing at the runtime buffer, or through
   `text_set` at `0x026625a0` for row 4), and A on it is caught on `f_02154abc`'s entry (A cleared in `S+4` for that call). The
   game's Gestures stays as it is otherwise.
2. **Or a third entry in Quit Game's second-level list,** if its drawing has spare panes: to check with Ghidra (the list's
   drawing code) and the probe (enter Quit Game, then back out).
3. A short probe run to finish the picture: switch to the Info tab, step its rows, enter Quit Game's list and back out with B,
   close, then open and close again.

### Proposed PRs

1. **Read-only probe (small):** hooks on `f_02154abc` and `f_0215249c` that log `S+0x20`/`0x24`/`0x25`/`0x28`/`0x1f` on
   change, and the label message ids. This confirms the offsets and which tab and row is which. One run: open the start menu, move
   through each tab and row, and close it.
2. **"Mods" row (opt-in, `MH3U_NATIVE_MENU=1`):** count 6 to 7 on tab 0, record 6, the label through `text_set`, and A logs
   "mod row selected" without leaving the menu.
3. **Mod list:** A opens the second-level list with 2 or 3 mod rows (e.g. "Wishlist status", "Hide overlay"). Actions run in the
   runtime; the game sees an unused action id that a hook catches.
