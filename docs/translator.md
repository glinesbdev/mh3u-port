# The translator: how it is tested

The translator turns the game's PowerPC code into C (see `pc-port.md`). The risk is not in writing it but in **getting a thousand
small meanings exactly right**: what `srawi` does to the carry bit, what `divw` gives for a zero, what `fmadds` does with a NaN. A mistake
there makes a game that runs and is subtly wrong. So the meaning of each instruction is checked against something that is not this
project's understanding of it.

## The reference interpreter (`mh3u-ppc`, `interp.rs`)

One core, no interrupts, no address translation: the *meaning* of each instruction in plain Rust, small enough to read against the
architecture book. It is what everything else is compared with. Not modelled: the floating point status register's exception bits (the
game's code never touches the register: spike 2), the hardware's estimate tables for `fres` and `frsqrte` (the exact reciprocal is used,
within the hardware's stated error), and the privileged and 64-bit instructions (the game uses none).

## QEMU as the oracle (`crates/mh3u-ppc/tests/`)

`qemu-ppc-static` (QEMU's user-mode PowerPC emulator, `-cpu 750`) is an independent implementation of the instruction set. The tests
build a small static 32-bit big-endian ELF for each case, with no linker: a stub loads a whole register state (32 GPRs, 32 FPRs, CR, XER,
LR, CTR and 256 bytes of scratch memory) from an array, runs the instruction under test, saves the state, and loops over the cases; at the
end it writes the arrays to standard output. The same states are run on the interpreter and the results compared bit for bit.

- `qemu_sanity.rs`: the harness itself (a known program, the state round trip, the loader).
- `qemu_diff.rs`: **every instruction** (the 1,250 or so words are random operands for each of about 150 instructions, 24 random states each
  with values chosen from the edge cases: 0, 1, -1, 0x7fffffff, 0x80000000, shift counts of 31, 32, 33, 63, 64; for floating point
  zeros, infinities, denormals, NaNs), every branch form with every kind of `BO`, the reservation pair, and the estimate instructions.
  The tests skip when QEMU is not installed.
- What it found in the interpreter: `lswi`/`lswx` cleared a register it should not have touched, and `lfs` made a signalling NaN quiet where the
  hardware only moves the fields. Both are fixed; the rest agreed from the start.

What QEMU does not check: the **paired singles** (QEMU has none), and the instructions it treats as illegal on the 750 (`fsqrt`, `fsqrts`:
the game does not use them). Their meaning has to come from the architecture book, and a second check (from the game itself: Cemu's
memory, see `docs/spikes.md`) is planned.

## The C against the interpreter (`crates/mh3u-testkit`, `crates/mh3u-recomp`)

`mh3u-recomp` writes C for each function: every instruction is a few statements using the helpers of `ppc.h` (the registers in a `Ctx`,
loads and stores through a base pointer, the carry and overflow rules, the condition register). `tests/c_diff.rs` translates every
instruction word of the same random cases, compiles them all into one program with the system C compiler (clang, cc or gcc), runs the
program on the same states and compares with the interpreter bit for bit: 1,320 words, every branch form that stays inside a
function, and the reservation pair. Together with the first part this is a chain: translated C = interpreter = QEMU.

Where the C is deliberately not the interpreter: a `bl` or `bctrl` is a *call* (a C call, or a hook for an address the translator does
not know) and `blr` is a *return*, where the interpreter changes the program counter; code that uses them as jumps into the middle
of something else is the translator's job to find (see "What is not translated yet").

## What is not translated yet

- The table for indirect calls (30,605 `rt_call(c, t)` sites) and for the 4,312 indirect tail jumps that are not switches: both go through
  the runtime, which must map an address to a translated function.
- The imports: a call to one of the 568 import trampolines (placed after the end of `.text`, from 0x31963d8) is `rt_import(c, number)`,
  and `imports.h` names each number ("library:function"). Which import a trampoline stands for comes from the relocations of `.text`
  (7,231 name an import; each trampoline serves one import, checked); the runtime implements `rt_import`. Imports reached through a
  pointer instead of a call are not known yet.
- Not found: functions only reached by a tail jump, and the 6 words in the code that are not instructions (data between functions).

## Switch tables (`switch_tables` in `mh3u-recomp`)

The game's compiler makes a `switch` as: compare the index (`cmplwi`), branch to the default when above, shift the index by two, add it
to an address built by `addis`/`addi` that is the address *right after the `bctr`*, `mtctr`, `bctr`; after the `bctr` comes a table of
`b` instructions. 510 of the 4,822 `bctr` are of this kind (503 with a table length equal to the `cmplwi` bound; the translator takes
the smaller of the bound and the run of branches). The C is a `switch` on the index with a `goto` to each branch's label, and it is
checked against the interpreter in `c_diff.rs`. The other 4,312 `bctr` are tail calls through a function pointer or a vtable (a
`lwz`/`mtctr` chain), not tables.

## The whole executable (`mh3u-tools translate`, `build-game`, `run`)

The 82,209 functions become 412 C files of 200 functions (247 MB, written in 2 seconds, outside any git tree) with `game.h`
(declarations), `imports.h` (the names of the system functions), and `dispatch.c`: the table that finds a function by address (for the
30,605 indirect calls and the indirect tail jumps), the table of import trampolines (an import called through a pointer reaches the
host as an import), and the hooks that route `rt_*` to the host. Every file passes `clang -fsyntax-only`.

`build-game` compiles the files on every core into a shared library (`libgame.so` / `game.dll`): 41 s at `-O0` on 24 cores.
`run` is the first runtime (`mh3u-rt`, the only crate with `unsafe`): it reserves the guest's 4 GiB address space, puts the
executable's sections in it, loads the library, gives it a stack and calls the entry point. Every system function is for now a
stub that returns 0 and is logged in order of first call. First result: the game's startup runs through 36 distinct system
functions (heap, mutexes, file system, save, system config) and then calls a null pointer, since the heap allocator returned 0.
The list of functions that need real code, in the order the game wants them, is the to-do list for the Cafe OS layer.
