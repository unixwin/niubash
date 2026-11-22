# WinuxCmd -- provenance

- Component: WinuxCmd v1.2.2 -- the C++ toolchain providing the
  GNU-compatible external command applets (`sed`, `cp`, `grep`, `awk`
  forwarder, ...). Bundled into every Windows release per
  `scripts/release/winuxcmd-pin.json`
  (SHA-256 `3f5f02905744598f8b307e6f32d2688c928b82845764876bba3f8127aa875995`
  windows-x64, `abcf7e894544273fe2ec6db5b9b0da4ba36d5f764ae918f4e7e666bfb241117b`
  windows-arm64, over the `WinuxCmd-*-win-<arch>.zip` release assets).
- Status: original MIT C++ software of the unixwin team -- not a port or
  redistribution of another codebase. The bundled `LICENSE` is copied
  verbatim from the WinuxCmd repository (`LICENSE` at
  `https://github.com/unixwin/WinuxCmd`, Copyright (c) 2026
  [caomengxuan666]). Before this directory existed, the only in-repo
  assertion of WinuxCmd's MIT/original status was a planning note; the
  license text here is the durable record.
- Binary origin: the WinuxCmd GitHub release matching the pin tag --
  `https://github.com/unixwin/WinuxCmd/releases/tag/v1.2.2`, downloaded
  and verified by the release workflow (sha256 per winuxcmd-pin.json).
- Corresponding Source: the WinuxCmd repository itself at the pinned tag
  (public GitHub repository; MIT does not impose a source-delivery
  obligation beyond retaining the copyright and license notice, which this
  file and the bundled `LICENSE` provide).
