# gawk -- provenance

- Component: GNU Awk (gawk) 5.4.1, wpm revision `unixwin2` (full five-DLL
  payload; same bytes as the v1.4.0-era full artifact).
- Version pin: `scripts/release/preinstall.json` (`gawk` 5.4.1-unixwin2),
  artifact SHA-256 `25373ae83ec539dbe8b4208e1b4c1afa891f902df1a19138ecda5451ccded4b6`
  (windows-x64; no arm64 artifact exists -- the arm64 release leg ships
  without gawk by design).
- Binary origin: wpm package `gawk`, artifact
  `https://github.com/unixwin/wpm-artifacts/releases/download/gawk-5.4.1-unixwin2/gawk-5.4.1-unixwin2-win-x64.zip`
  (mirrored through unixwin/wpm-artifacts; the release notes for
  `gawk-5.4.1-unixwin2` record that this asset is byte-identical to the
  verified-good full `gawk-5.4.1` artifact -- see wpm-artifacts#2 for the
  unixwin1 transitive-DLL regression that forced the reissue).
- Upstream build source: repackaged mirror of the ezwinports build
  `https://sourceforge.net/projects/ezwinports/files/gawk-5.4.1-w32-bin.zip`
  (32-bit mingw build, runs on x64 via WOW64), per the wpm-artifacts release
  notes for `gawk-5.4.1` / `gawk-5.4.1-unixwin1` / `gawk-5.4.1-unixwin2`.
  The upstream URL is recovered from those release notes and the
  unixwin/wpm-source `index.json` `gawk` entry ("Mirrored from ezwinports").
- License: GPL-3.0-or-later -- text in `LICENSE` (GNU GPL v3).
- Corresponding Source: the official GNU gawk 5.4.1 source distribution
  (`https://ftp.gnu.org/gnu/gawk/`, the 5.4.1 archive). The ezwinports
  packaging recipe (any build patches it applied) is upstream-side and is
  not recorded in wpm-artifacts; the byte-level guarantee this repo makes is
  the SHA-256 pin above, and the GPL source offer points at GNU's official
  gawk 5.4.1 source for the same version. If a rebuild-for-source offer is
  ever needed, start from that archive plus the ezwinports project.
- Note (niubash#467): the payload keeps its shipped text-mode-build bytes;
  the LF behavior contract lives in the bundled WinuxCmd forwarder
  (`-v BINMODE=3` argv splice), not in this payload.
