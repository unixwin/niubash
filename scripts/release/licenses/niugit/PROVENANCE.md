# niugit -- provenance

- Component: niugit 2.56.0.3 -- real git for Windows without MSYS: the
  git-for-windows C source built with MSVC, no POSIX/MSYS runtime layer.
- Version pin: `scripts/release/preinstall.json` (`niugit` 2.56.0.3),
  SHA-256 `fc700ae92fb50209344ad7d18649e1fc1cb5cba21ffe75d95ec5668fd1e625a1`
  (windows-x64), `490fc3c2abf5da6b5441b8717ce8f22d187b74cd7f04aed6abc4ed50f9f4b12f`
  (windows-arm64).
- Binary origin: unixwin/niu-git release v2.56.0.3 --
  `https://github.com/unixwin/niu-git/releases/download/v2.56.0.3/niu-git-2.56.0.windows.4-x64.zip`
  and `...-arm64.zip`. 2.56.0.3 is the Git for Windows 2.56.0.windows.4
  security rebuild (brotli 1.2.1, fixing CVE-2026-111497).
- License: GPL-2.0-only (git's own license) -- text in `LICENSE` (GNU GPL v2).
- Corresponding Source recipe (all pinned in the unixwin/niu-git
  repository):
  1. Upstream tarball:
     `https://codeload.github.com/git-for-windows/git/tar.gz/refs/tags/v2.56.0.windows.4`
     -- byte size 12,871,354, SHA-256
     `34f83d6af52f09b997e295db337ede5bbd1613db4405c4b32a05fc9c5363758c`,
     verified and pinned by `scripts/build.py`
     (`GIT_TAG` / `GIT_TARBALL_SIZE` / `GIT_TARBALL_SHA256`) at niu-git
     origin/master commit 9df37d2 ("build: bump upstream baseline to Git for
     Windows 2.56.0.windows.4", #20).
  2. Distribution patches: `patches/0001-0006` in the same repository
     (http stale-sslBackend tolerance, POSIX-shaped path argument
     acceptance, system-prefix relocation for the wpm flat layout,
     build-options stamping, clean no-shell failure, shebang-interpreter
     diagnostics).
  3. Build script: `scripts/build.py` in the same repository (extract,
     patch, configure, compile with MSVC, stamp `NIUGIT_BUILD_VERSION`).
  The tarball + patches + build script reproduce the shipped binary; that
  trio is the GPL-2.0 Corresponding Source offer for this component.
- Footnote: the niu-git checkout on this workspace machine is one baseline
  behind (v2.56.0.windows.3 @ ec953aa = the 2.56.0.2 build); the windows.4
  pin cited above is from origin/master 9df37d2, the revision that produced
  the pinned 2.56.0.3 release.
