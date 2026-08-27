# Command-Not-Found Provider ABI Draft
This draft defines the first non-command provider target for plugin
externalization. It is now a design gate and the contract for the current process-provider
runtime.
## Goal
Move `command-not-found` from host-only builtin behavior toward a provider
contract where a process pack can now receive deterministic missing
command context and return suggestion lines. Do not reuse ordinary command
execution as a hidden provider contract; provider output capture, timeout
handling, and fallback behavior must remain explicit host behavior.
Process packs may use `exports.providers = ["command-not-found"]` with `command:diagnose` as a
guarded provider binding. The official bundle can still keep this pack builtin;
that marker does not replace the host builtin.
## Current Native Behavior
Today Winuxsh generates missing-command diagnostics inside the host:
- base line: `winuxsh: <command>: command not found`;
- optional WPM install hint for known command/package mappings;
- optional package-manager search hints when `winget`, `scoop`, or `choco` are
  available.
The host owns command discovery and package-manager detection. A provider ABI
must preserve deterministic output and must not execute suggested commands.
## Process Provider Contract
Provider name:
- `command-not-found`
Required permission:
- `command:diagnose`
Input:
- missing command string;
- shell-visible cwd when `cwd:read` is granted;
- optional host facts, such as whether package search helpers are available;
- optional command args after the missing command, if later needed for richer
  suggestions.
Output:
- ordered UTF-8 suggestion lines;
- no shell mutations;
- no direct writes to user stdout/stderr except host-owned final rendering;
- deterministic empty output is allowed and means "fall back to host defaults".
Failure behavior:
- timeout, trap, invalid UTF-8, oversized output, or provider runtime failure
  falls back to the compiled host implementation;
- provider failure must not replace the base `command not found` diagnostic;
- provider stderr is diagnostic-only and should be suppressed or debug-logged by
  default.
## Current Host Binding
Status: process-provider binding implemented.
Winuxsh now has tested helpers for:
- building a provider request from the missing command, optional args, optional
  cwd, and available package-search helpers;
- parsing provider output as bounded UTF-8 suggestion lines;
- treating empty output, invalid UTF-8, oversized output, and runtime failure as
  fallback to the compiled native hints;
- preserving the base command-not-found diagnostic before
  any provider or fallback suggestions.
The shell now invokes enabled process providers for this surface from command
mode. The compiled native implementation remains the fallback renderer.
## Open Decisions
- Manifest representation beyond today's guarded `exports.providers` marker:
  provider-specific tables, versioning, output limits, and runtime binding.
- Runtime representation: keep the current process provider binding or add a
  host-owned bridge when a provider should stay in core.
- Output framing: newline-delimited text, length-prefixed records, or JSON.
- Host facts: whether package-manager availability is passed as booleans, a
  compact string map, or separate host imports.
- Fallback ordering: provider suggestions before host WPM hints, after them, or
  provider-only with compiled fallback on empty output.
## Definition Of Ready
- Provider invocation is separate from command execution.
- Provider permissions are visible in `plugin review`.
- Provider timeout and invalid output fall back to host defaults.
- Tests prove base diagnostics still appear when the provider is absent,
  disabled, failing, timing out, or returning no suggestions.
