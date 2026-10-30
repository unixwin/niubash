# dll-closure -- provenance (DLLs shipped inside the gawk payload)

These five DLLs land in `<root>\opt\gawk\` alongside `gawk.exe` because the
pinned gawk payload's import closure requires them (`files` list of the wpm
`gawk` package entry in `scripts/release/preinstall.json`; the unixwin2
revision exists precisely because unixwin1 dropped two transitively-imported
DLLs and gawk died with 0xC0000135 on clean machines -- wpm-artifacts#2).
All five ship as the exact bytes inside the pinned gawk zip
(SHA-256 `25373ae83ec539dbe8b4208e1b4c1afa891f902df1a19138ecda5451ccded4b6`,
mirrored from ezwinports via unixwin/wpm-artifacts -- see `../gawk/PROVENANCE.md`).

| DLL | Project | License | Text |
| --- | --- | --- | --- |
| `libreadline8.dll` | GNU Readline | GPL-3.0-or-later | `LICENSE-GPL-3.0.txt` |
| `libgmp-10.dll` | GMP (GNU Multiple Precision Arithmetic Library) | dual: LGPL-3.0 / GPL-2.0 (GMP's standard dual license) | `LICENSE-LGPL-3.0.txt`, `LICENSE-GPL-2.0.txt` |
| `libmpfr-6.dll` | MPFR (GNU MPFR) | LGPL-3.0 | `LICENSE-LGPL-3.0.txt` |
| `libncurses6.dll` | ncurses | MIT-style (ncurses COPYRIGHT NOTICE) | `LICENSE-NCURSES.txt` |
| `libgcc_s_dw2-1.dll` | GCC runtime (libgcc) | GPL-3.0 + GCC Runtime Library Exception 3.1 | `LICENSE-GPL-3.0.txt` + `LICENSE-GCC-RUNTIME-EXCEPTION-3.1.txt` |

Upstream project homes:

- GNU Readline: `https://tiswww.case.edu/php/chet/readline/rltop.html`
- GMP: `https://gmplib.org/`
- GNU MPFR: `https://www.mpfr.org/`
- ncurses: `https://invisible-island.net/ncurses/` (the notice in
  `LICENSE-NCURSES.txt` is the license statement published by the ncurses
  maintainer at `https://invisible-island.net/ncurses/ncurses-license.html`)
- GCC: `https://gcc.gnu.org/`

Corresponding Source for each DLL is the upstream project's own source
distribution for the version compiled into the ezwinports binaries; this
repo records no independent build recipe for the DLLs (they are consumed
unmodified from the pinned ezwinports payload). The ncurses license requires
that its COPYRIGHT NOTICE accompany any distribution and its documentation --
that is what this directory and the shipped `licenses\dll-closure\` copy do.
