# Input: gamepads, keyboard and mouse

Everything feeds one virtual Wii U GamePad (`mh3u-rt/src/input.rs`), which the game reads with `VPADRead`. A gamepad and the
keyboard and mouse work together at any time:
- **Buttons:** pressing a button by either counts.
- **Sticks:** a pushed gamepad stick wins; otherwise the keyboard's and the mouse's add up.

## Keyboard and mouse (PR #12)

**Off by default since PR #34** (they were on before): `MH3U_KEYBOARD_CONTROLS=1` (`win-run.ps1 -KeyboardControls`,
`run.sh --keyboard-controls`) turns them on. `MH3U_KBM` is no longer read (the log says so when it is set).

Without them, the window never grabs or hides the cursor, the mouse does not turn the camera, no key moves the hunter or presses a
GamePad button (the native menu's mod key included; ZL on a pad does the same), and the mouse's left button touches the GamePad's
screen. What still works without them, with the window focused:
- **F1** opens and closes the overlay, and the open overlay takes the mouse and keyboard as always: clicking, scrolling, typing in
  its search and text fields (the naming keyboard too).
- **F11** and **Alt+Enter** (fullscreen), **F9** and **F10** (the hook-finding tools' keys, `hooks.md`), and closing the window.

With them on:
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
- **Right Shift** is left unbound by default: it is the opt-in native Mods entry's key (`MH3U_NATIVE_MENU`, `native-menu.md`), and like
  the other keys it counts only with keyboard controls on.
  On a gamepad, **ZL** held does the same. With that opt-in on, ZL is kept from the game while the start menu or the Mods page
  is up, so it cannot trigger anything there. It works as usual everywhere else.
- **The left stick:** a diagonal goes as far as a straight push (it is normalised), and the walk key scales it by `walk_scale`.
- **The right stick** follows the mouse's speed, from raw motion (unaffected by the pointer's acceleration or the window's edges):
  - `mouse_sensitivity` is stick units per pixel a second. The default reaches full deflection at about 670 pixels a second.
  - The stick eases towards that with a time constant of `mouse_smoothing_ms`, so it also falls back to exactly 0 that fast when
    the mouse stops. 0 means no smoothing.
  - No dead zone is added (the game has its own). `invert_y` makes moving the mouse up push the stick down.

### Tested (after PR #12)

The user's run: the keyboard config works well, and remapping the bindings and the camera settings in `bindings.json` worked
perfectly. It is a little awkward to play only because MH3U's controls are made for a controller: the attacks are face buttons,
the camera has no free look, and the item bar and map are on the touch screen.

Possible later, with translation-time hooks (`roadmap.md`): mouse-friendly features the GamePad can't express, such as
- free mouse-look written straight into the camera's angles, instead of going through the right stick;
- scrolling the item bar with the mouse wheel;
- touch-screen actions on keys;
- target lock on a mouse button.

## Gamepads

Read with gilrs on a thread of their own. `MH3U_PAD_LAYOUT=nintendo` swaps A with B and X with Y, and `MH3U_GAMEPADS=0` turns them
off. **Back (View) + RB** opens the overlay (see `overlay.md`).

## While the window is in the background (issue #33)

Two separate settings, so a pad can keep playing while someone else uses the PC:

| Setting | Default | What it does |
|---|---|---|
| `MH3U_BACKGROUND_GAMEPAD` (`win-run.ps1 -NoBackgroundGamepad` / `-BackgroundGamepad`, `run.sh --no-background-gamepad` / `--background-gamepad`) | **on**, and **off** when `RECOMPLAUNCHER=1` unless the variable is on | Gamepads keep playing when the window does not have the focus. `0` (or `off`) keeps them to the focused window. RecompLauncher sets `RECOMPLAUNCHER=1`; its overlay uses the controller, so background input stays off until you turn it on. |
| `MH3U_BACKGROUND_KEYBOARD` (`-BackgroundKeyboard`, `--background-keyboard` set it to `1`) | off | With keyboard controls on (`MH3U_KEYBOARD_CONTROLS=1`), the keyboard's bindings also play without the focus. Opt-in, so typing elsewhere does not move the hunter. Without keyboard controls it does nothing (the log says so). |

- **The mouse** never plays in the background: it is only the right stick while the cursor is grabbed, which needs the focus.
- **Windows:** gilrs's backend (Windows.Gaming.Input) hears nothing while no window of the process has the focus, so without the
  focus the pad is read with **XInput** instead (`input.rs`, `background`). That covers Xbox and other XInput pads (and pads that
  Steam Input or DS4Windows show as one); a PlayStation or Switch pad on its own plays only with the focus. The first connected of
  XInput's four slots is used. The log says `[gamepad] window in the background: reading the pad with XInput` and `… back to
  gilrs` on each change; a button held across a focus change is let go and has to be pressed again. Back + RB still opens the
  overlay from the background.
- **Linux:** gilrs (evdev) hears pads whatever has the focus, so this only changes anything with `MH3U_BACKGROUND_GAMEPAD=0`.
  Background keys come from winit's raw device events: X11 gives them, Wayland gives none to a window without the focus.
- **Keys:** without the focus the raw keys are used (only the game's bindings and the native menu's mod key; F1, F11, F9 and the
  like do nothing). Keys held when the window gets the focus back are let go, and the window's own key events take over.
- The startup log says which way each is set: `gamepads also work while the window is in the background …` and `[input] the
  keyboard … while the window …`.
- Not yet: a switch in the overlay or the native menu.
