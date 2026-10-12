# scripts/adversarial/ — the injection toolkit

Reusable fault injectors for the adversarial matrix
([docs/audit/adversarial-matrix.md](../../docs/audit/adversarial-matrix.md)).
Each tool is
self-contained (stdlib only, PowerShell 5.1 compatible, no admin unless
stated), takes `--log-file`, and is sandbox-safe: nothing here touches a
real profile or the network outside 127.0.0.1.

| Tool | Axis | What it injects | Demo |
| --- | --- | --- | --- |
| `reset-proxy.py` | 1 hostile network | TCP/TLS resets: at handshake, mid-transfer (drop the crossing chunk), after N seconds, every Kth connection; clean `normal` baseline | `logs/reset-normal.log`, `logs/reset-midclone.log` |
| `throttle-git.py` | 9 timing + 1 | 10 KB/s slow git (pay-before-forward pacing — verified 7.9 KB/s), captive-portal `--stall-after-bytes`, `--mode blackhole` (connect succeeds, no data) | `logs/throttle.log`, `logs/blackhole.log`, `logs/stall.log` |
| `diskfull-vhd.ps1` | 6 resource exhaustion | a disposable 200 MB NTFS volume (diskpart; `New-VHD` tried first), `Fill` until real ENOSPC (HRESULT 0x80070070), `Status`, `Remove` | demo transcript in the matrix doc §3 |
| `state-corrupter.py` | 2 aged state, 8 upgrade | week-old drift in the REAL file shapes: mixed CRLF, duplicated managed blocks, approximate markers, hand lines inside blocks, stale `bootstrap-failures.toml`, damaged/partial trees, registry attribution flips, broken TOML, pre-1.3.1 imperative-era state; `--op restore` rolls back | demo transcript in the matrix doc §3 |
| `env-poisoner.py` | 7 weird-but-legal input | poisoned child environments: Git-Bash `PS1`+`MSYSTEM` (#117), HOME/USERPROFILE split or both unset, 64 KB values, CRLF+unicode values, `BASH_ENV` probe file, garbage `NIU_*` | demo transcript in the matrix doc §3 |
| `file-locker.ps1` | 6 resource exhaustion | FileShare::None hold on a file (AV-style); `-Test` reports lockability (rc 9 = locked) | demo transcript in the matrix doc §3 |

## Deterministic offline git fixture (how the proxies are driven)

Both proxies are HTTP proxies git accepts via `-c http.proxy=http://127.0.0.1:PORT`
(CONNECT tunnels for https; absolute-form requests for plain http — both
forwarded). Lanes run them against a local dumb-HTTP git remote so the
failure is fully deterministic and offline:

```sh
git init --bare demo-remote.git && git update-server-info
python -m http.server 18099 --bind 127.0.0.1 --directory <dir-with-demo-remote.git>
python scripts/adversarial/reset-proxy.py --port 8888 --mode reset-after-bytes --bytes 2048 &
git -c http.proxy=http://127.0.0.1:8888 clone http://127.0.0.1:18099/demo-remote.git wt
kill %1   # and the http.server when done — no stuck processes
```

Verified shapes (demo, 2026-10-02): baseline clone through `normal` = rc 0;
`reset-after-bytes 2048` = `error: fetch failed` rc 128 with an empty
worktree; `--rate-kb-per-s 8` = 7.9 KB/s per connection; blackhole +
`http.lowSpeedLimit=1/lowSpeedTime=3` = `Operation too slow` rc 128 in 4 s;
`--stall-after-bytes 200` = same bounded failure after partial data.

## Design rules the tools follow

1. **The kit injects; the lane asserts.** Tools never touch product state
   beyond the sandbox HOME a lane hands them; assertions live in lane
   probes (`scripts/adversarial/probes/<lane-id>/`).
2. **Deliberate kills RST; clean closes stay clean.** The proxies only
   send RST on a chosen fault path (SO_LINGER 0); normal connection ends
   drain and FIN. A control run through `--mode normal` must clone fine —
   if it doesn't, the fixture is broken, not the product.
3. **Real file shapes.** state-corrupter writes the exact marker pair
   (`# >>> niu source <id> (managed by …) >>>`), spec schema
   (`niubash:plugin-spec@0.1.0`), registry schema
   (`niubash:plugin-source-registry@0.3.0`) and bootstrap-failure schema
   (`niubash:plugin-bootstrap-failures@1`) read from the product source —
   lanes assert against shapes the product actually parses.
4. **Everything backs up before mutating** (`.pre-corrupt`) and
   `--op restore` undoes a round, so a lane can heal state between probes.
5. **No stuck processes.** Proxies take `--hold-seconds` bounds; lanes
   must kill every background fixture (proxy, http.server, locker) before
   finishing a turn.
