# MAN-22 — Harden `tunnel_e2e` orphan assertion (`pgrep -f` false positives)

## What changed

`crates/tunnel-core/tests/tunnel_e2e.rs`:

1. Replaced the AFTER check `pgrep -f cloudflared` with a process-**name** check
   `pgrep -x cloudflared` **plus** a known-pid check (`/proc/<child pid>`).
   `-f` matched the whole command line, so the invoking shell/wrapper was
   reported as a false orphan.
2. BEFORE now snapshots cloudflared pids by name; DURING and AFTER use the same
   name-based check, so all three phases are consistent.
3. AFTER polls with a bounded 5 s retry (100 ms step) instead of a fixed 500 ms
   sleep, because reaping can be asynchronous.
4. Added `orphan_check_detects_a_real_named_orphan` (unix, runs in the normal
   `cargo test`): copies a long-running binary to a file named `cloudflared`,
   spawns it, and asserts the orphan detector flags it — proving the check still
   fails on a real orphan.

Environment: `cargo 1.99.0`, cloudflared at
`/home/datnt/.local/share/com.paperclip.portforward/cloudflared`.

## 1. Reproduction of the original false positive (BEFORE fix)

Original test restored via `git checkout`, invoked from a wrapper shell whose
command line contains the string `cloudflared`
(`bash -c '... CLOUDFLARED_BIN=.../cloudflared TUNNEL_E2E=1 cargo test ...'`):

```
thread 'start_returns_live_url_and_stop_leaves_no_orphan' panicked at tunnels_e2e.rs:73:5:
no orphan cloudflared expected, found: 92699
92700
test start_returns_live_url_and_stop_leaves_no_orphan ... FAILED
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 3.95s
```

The reported "orphans" `92699`/`92700` were the wrapper shells, not cloudflared:
`pgrep -x cloudflared` returned none.

## 2. AFTER fix — same wrapper whose command line contains `cloudflared`

```
running 1 test
test start_returns_live_url_and_stop_leaves_no_orphan ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 3.19s
```

Also passes directly:

```bash
TUNNEL_E2E=1 CLOUDFLARED_BIN=/home/datnt/.local/share/com.paperclip.portforward/cloudflared \
  cargo test -p tunnel-core -- --ignored
```

## 3. Negative case — real orphan must still fail the test

Temporarily replaced `mgr.stop().expect("stop should succeed");` with a no-op to
leak the child, then ran the gated test:

```
thread 'start_returns_live_url_and_stop_leaves_no_orphan' panicked at tunnel_e2e.rs:132:9:
no orphan cloudflared expected, but child pid 93353 still alive: true; new cloudflared pids: [93353]
test start_returns_live_url_and_stop_leaves_no_orphan ... FAILED
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.81s
```

(Later restored; no leftover `cloudflared` remained — `TunnelManager::drop` reaps.)

The dedicated helper test also verifies this without the network:

```
test orphan_check_detects_a_real_named_orphan ... ok
```

## 4. Default (non-gated) suite

```
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out (unit tests)
test result: ok. 1 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out (tunnel_e2e.rs)
```
