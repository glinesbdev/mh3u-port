# Input: gamepads, keyboard and mouse

Everything feeds one virtual Wii U GamePad (`mh3u-rt/src/input.rs`), which the game reads with `VPADRead`. A gamepad and the
keyboard and mouse work together at any time:
- **Buttons:** pressing a button by either counts.
- **Sticks:** a pushed gamepad stick wins; otherwise the keyboard's and the mouse's add up.

## Keyboard and mouse (PR #12)

On by default; `MH3U_KBM=off` turns them off. The keyboard then does nothing, and the mouse's left button touches the GamePad's
screen, as before.
- **The bindings** are in `<build dir>/input/bindings.json`, made with the defaults on the first run. Edit it and restart. Fields
  left out keep their defaults. A file that cannot be read gives the defaults and one `[input]` line saying why; the file is left
  as it is.
- **The cursor is grabbed** (locked to the window and hidden) while playing. It is let go while the overlay is open, and whenever
  the window loses the focus. The click that gives the window the focus back does not press a button.
- **F1** (the overlay), **F11** and **Alt+Enter** (fullscreen) belong to the window. They cannot be bound, and Enter is not bound
  either. While the overlay is open, the keyboard and mouse go to it only.
- **Controls window:** the overlay has one listing the bindings (read-only).

### Default bindings

The GamePad's buttons are positional (what each does in the game depends on the weapon and the menu; see the game's own controls
screen). The defaults put the attack buttons on the mouse and the rest near WASD:

| GamePad | Keyboard / mouse | | GamePad | Keyboard / mouse |
|---|---|---|---|---|
| Left stick | W A S D (Left Ctrl: walk, half way) | | Right stick | mouse |
| X | left mouse button | | A | right mouse button |
| B | Space | | Y | E |
| R | Left Shift | | L | Q |
| ZR | R | | ZL | F |
| + | Escape | | − | Tab |
| D-pad | arrow keys, or 1 (up) 2 (down) 3 (left) 4 (right) | | Home | Home |
| L3 (left stick press) | C | | R3 (right stick press) | middle mouse button |

Before this PR the keyboard had a fixed layout: Enter A, Backspace B, X X, C Y, IJKL the D-pad, Q L, E R, 1 ZL, 3 ZR, P +, O −,
Shift L3 and R3, and the arrows the right stick. The file replaces it.

### The file

```json
{
  "buttons": { "A": ["MouseRight"], "B": ["Space"], "X": ["MouseLeft"], "...": [] },
  "move_up": "KeyW", "move_down": "KeyS", "move_left": "KeyA", "move_right": "KeyD",
  "walk": "ControlLeft", "walk_scale": 0.5,
  "mouse_sensitivity": 0.0015, "mouse_smoothing_ms": 30.0, "invert_y": false
}
```

- **Button names:** `A B X Y L R ZL ZR Plus Minus Home Up Down Left Right L3 R3`. Each takes a list of inputs. Binding a button
  replaces the whole `buttons` map, so list all the buttons wanted.
- **Keys** are named by position as winit names them (`KeyQ`, `Digit1`, `Space`, `ShiftLeft`, `ControlLeft`, `Tab`, `Escape`,
  `ArrowUp`...), so they do not move with the keyboard layout. Mouse buttons are `MouseLeft`, `MouseRight`, `MouseMiddle`,
  `MouseBack` and `MouseForward`.
- **The left stick:** a diagonal goes as far as a straight push (it is normalised), and the walk key scales it by `walk_scale`.
- **The right stick** follows the mouse's speed, from raw motion (unaffected by the pointer's acceleration or the window's edges):
  - `mouse_sensitivity` is stick units per pixel a second. The default reaches full deflection at about 670 pixels a second.
  - The stick eases towards that with a time constant of `mouse_smoothing_ms`, so it also falls back to exactly 0 that fast when
    the mouse stops. 0 means no smoothing.
  - No dead zone is added (the game has its own). `invert_y` makes moving the mouse up push the stick down.

## Gamepads

Read with gilrs on a thread of their own. `MH3U_PAD_LAYOUT=nintendo` swaps A with B and X with Y, and `MH3U_GAMEPADS=0` turns them
off. **Back (View) + RB** opens the overlay (see `overlay.md`).
