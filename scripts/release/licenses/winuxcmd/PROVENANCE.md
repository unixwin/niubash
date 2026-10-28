# WinuxCmd -- provenance

- Component: WinuxCmd v1.1.20 -- the C++ toolchain providing the
  GNU-compatible external command applets (`sed`, `cp`, `grep`, `awk`
  forwarder, ...). Bundled into every Windows release per
  `scripts/release/winuxcmd-pin.json`
  (SHA-256 `790d1f6eef3a555fd2c043866d32c6edbf936a8a70d25e554b3383e67b36ad43`
  windows-x64, `205edb87a06b8b5b3c6fb731884d2b91b60fe406b9e7f3dbe625326489bb89df`
  windows-arm64, over the `WinuxCmd-*-win-<arch>.zip` release assets).
- Status: original MIT C++ software of the unixwin team -- not a port or
  redistribution of another codebase. The bundled `LICENSE` is copied
  verbatim from the WinuxCmd repository (`LICENSE` at
  `https://github.com/unixwin/WinuxCmd`, Copyright (c) 2026
  [caomengxuan666]). Before this directory existed, the only in-repo
  assertion of WinuxCmd's MIT/original status was a planning note; the
  license text here is the durable record.
- Binary origin: the WinuxCmd GitHub release matching the pin tag --
  `https://github.com/unixwin/WinuxCmd/releases/tag/v1.1.20`, downloaded
  and verified by the release workflow (sha256 per winuxcmd-pin.json).
- Corresponding Source: the WinuxCmd repository itself at the pinned tag
  (public GitHub repository; MIT does not impose a source-delivery
  obligation beyond retaining the copyright and license notice, which this
  file and the bundled `LICENSE` provide).
