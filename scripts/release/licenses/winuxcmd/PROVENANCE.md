# WinuxCmd -- provenance

- Component: WinuxCmd v1.2.3 -- the C++ toolchain providing the
  GNU-compatible external command applets (`sed`, `cp`, `grep`, `awk`
  forwarder, ...). Bundled into every Windows release per
  `scripts/release/winuxcmd-pin.json`
  (SHA-256 `5d5a08a88705c0bcda55de5ab71497e849340aed362b3e5ef907c54bd67b7806`
  windows-x64, `af55bac48b3fa90787c6997ab975bd32c1934480ae4241ef1e4bd8330b6aa4ed`
  windows-arm64, over the `WinuxCmd-*-win-<arch>.zip` release assets).
- Status: original MIT C++ software of the unixwin team -- not a port or
  redistribution of another codebase. The bundled `LICENSE` is copied
  verbatim from the WinuxCmd repository (`LICENSE` at
  `https://github.com/unixwin/WinuxCmd`, Copyright (c) 2026
  [caomengxuan666]). Before this directory existed, the only in-repo
  assertion of WinuxCmd's MIT/original status was a planning note; the
  license text here is the durable record.
- Binary origin: the WinuxCmd GitHub release matching the pin tag --
  `https://github.com/unixwin/WinuxCmd/releases/tag/v1.2.3`, downloaded
  and verified by the release workflow (sha256 per winuxcmd-pin.json).
- Corresponding Source: the WinuxCmd repository itself at the pinned tag
  (public GitHub repository; MIT does not impose a source-delivery
  obligation beyond retaining the copyright and license notice, which this
  file and the bundled `LICENSE` provide).
