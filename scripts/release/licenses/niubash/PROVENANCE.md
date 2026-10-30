# niubash -- provenance

- Component: Niubash itself (the `niu` binary plus the bundled shell engine,
  rubash, which is a library dependency of this project and MIT-licensed in
  its own right at `https://github.com/unixwin/rubash`).
- License: MIT -- the repository root `LICENSE`, copied verbatim
  (Copyright (c) 2026 unixwin team).
- Not a third-party payload: included here so the archive's `licenses\`
  directory is self-contained and a recipient can read niubash's own terms
  next to those of everything bundled with it.
- Source: the tagged release commit of `https://github.com/unixwin/niubash`
  that produced the artifact (the tag equals the archive's version;
  `niu --version` also names the pinned rubash revision embedded by
  `build.rs`).
