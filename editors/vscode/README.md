# Niubash Debugger — zero-JS VS Code extension

A purely declarative extension: `package.json` only. There is no
JavaScript, no activation entry point, and no code to audit — the manifest
contributes a debugger of type `niubash` whose adapter is the Niubash
binary itself:

```
niu --debug --dap
```

VS Code spawns that command and speaks DAP over its stdio. The adapter is
the native debugger's DAP frontend, the same session core the interactive
`niu --debug` terminal frontend drives — one debugger, two views.

## Requirements

`niu` on `PATH` (the Niubash installation). With a program operand the
adapter serves that program; with none it runs as a pure DAP server and the
program arrives in the client's `launch` request.

## Launch configuration

The contributed `launch` attributes are exactly the ones the adapter
implements: `program` (required), `args`, `cwd`, `env`, `stopOnEntry`,
`noDebug`. `attach` is accepted only in its launch-equivalent form (a
`program` argument): background jobs are separate OS processes and cannot
be attached.

## Install

Copy or symlink this directory into your VS Code extensions directory, or
package it with `vsce package` from this directory. The manifest has no
runtime dependencies.
