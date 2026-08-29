# Native builtins: retired inventory

Status: retired.

Niubash no longer owns shell-command or coreutils-style builtin implementations.
The old `native_file_builtins` runtime module was removed so command ownership is
not split across three layers.

Current ownership:

- Rubash owns shell builtins and shell semantics such as `source`, `pwd`,
  `setopt`, `unsetopt`, `command`, `builtin`, aliases, options, functions,
  redirects, pipelines, and job/control-flow behavior.
- WinuxCmd owns external Unix-style utilities such as `cat`, `chmod`, `cp`,
  `mkdir`, `mkfifo`, `rm`, `rmdir`, `touch`, `ls`, `tree`, and related
  coreutils commands through normal Windows PATH resolution.
- Niubash owns host integration: REPL, configuration, completion, prompt,
  plugins, PATH setup, command-not-found hints, Windows cwd/env sync, and
  startup/lifecycle hooks.

Historical note: this file used to track a uutils-style port of file helpers
inside Niubash. That approach made Niubash, Rubash, and WinuxCmd all implement
overlapping command behavior. The port was removed in favor of routing shell
builtins to Rubash and external commands to WinuxCmd.
