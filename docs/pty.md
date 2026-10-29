# PTY transport and lifecycle

Sources: [`pty.rs`](../src/pty.rs), [`pty/input.rs`](../src/pty/input.rs),
[`pty/unix.rs`](../src/pty/unix.rs),
[`pty/lifecycle.rs`](../src/pty/lifecycle.rs), and
[`app/pty_io.rs`](../src/app/pty_io.rs).

## Session ownership

Each tab owns a `PtySession` with a master handle, output receiver, input sender,
and lifecycle control. Two tracked workers read and write; one process-wide
supervisor owns child handles and worker completion. Dropping a child wrapper or
the UI's master alone cannot clean up worker-owned descriptors or a blocked write.

The shell comes from `[shell] shell`, otherwise `$SHELL` (`COMSPEC` on Windows)
with a platform fallback. An explicit `[shell] shell` is used verbatim. When
cometty runs from its macOS `.app` bundle, the auto-detected shell starts with
`-l`: Finder launches inherit launchd's minimal environment, and login profiles
(notably `/etc/zprofile` → `path_helper`) establish PATH. Unbundled runs and
non-macOS builds keep the non-login default. Shell command splitting supports
quotes/backslash grouping, not variable expansion or a shell-evaluated command
string. A failed spawn tries `/bin/bash` (with `-l` when the bundle default was
login) and logs the failure. TERM defaults to `xterm-256color`; cwd defaults to
`$HOME` or `/` (a restored tab passes its saved directory). Geometry uses the
same clamp as the terminal.

Children get `TERM_PROGRAM=cometty` and never inherit the identity of the
terminal that launched cometty (`TERM_SESSION_ID`, iTerm/kitty/WezTerm/tmux
variables, …; see `INHERITED_TERMINAL_ENV` in [`pty.rs`](../src/pty.rs)).
Inherited, macOS zsh would run `/etc/zshrc_Apple_Terminal` and every tab would
share the parent Terminal.app tab's session and history files.

## Output backpressure and coalesced wakes

The reader transfers at most **8 KiB per chunk** into a **128-message bounded
channel**, approximately 1 MiB per session. One additional reader send may be
blocked outside the channel; UI/kernel/parsed storage is separate. Backpressure
deliberately reaches the child when the UI cannot keep up.

One pending-notification flag coalesces wakes until the receiver observes empty.
Re-arming clears the flag **and rechecks the channel**: a producer racing the
first empty observation may have suppressed its wake. Removing that recheck can
strand output indefinitely. Budget exhaustion schedules continuation separately;
it must not depend on another producer notification.

The reader emits `PtyEvent::Exit` on EOF/read error **after** previously read data.
Exit delivery describes stream completion, not a replacement for child reaping.
The app routes clipboard operations before removing tabs, sorts removals, then
remaps active/fair-order indices. Stable session IDs survive index shifts.

## Fair draining

[`app/scheduling.rs`](../src/app/scheduling.rs) alternates active-tab service with
a persistent round-robin cursor. One visit consumes at most one output chunk.
The order survives deadline exhaustion, tab switches, and removals; restarting
at tab zero or always at the active tab would allow starvation.

`new_events` resets one **256 KiB / 4 ms** allowance per event-loop turn. All
drain call sites share bytes and accumulated drain-call elapsed time. It is a
**cooperative allowance**: a parser callback, process query, clipboard operation,
or resize can overshoot it. Smaller read chunks do not bound expanded escape
operations or long grapheme appends.

Remaining work sets `pty_pending`, waking another turn through `about_to_wait`.
Parsing/replies/exits continue during selection drags, synchronized output,
occlusion, and disabled blinking. Background output does not dirty the active
terminal or clear its selection; redraw chrome only if displayed labels change.
Process/title facts refresh once per touched tab/burst, not per chunk or paint.

Only output-driven repaint requests are paced/coalesced (see
[event-loop design](architecture.md#decisions-that-shape-the-event-loop)). Fair
draining, queue sizes and input/reply admission are unchanged. A drain immediately
before an already requested frame includes output in that frame without asking
for a redundant next frame. Input urgency bypasses the output deadline, not the
fair parsing budget or synchronized-output guard.

## Decision: nonblocking, byte-accounted input admission

Blocking the UI on a full input channel is unacceptable; an unbounded paste queue
is equally unsafe. Admission in `pty/input.rs` is synchronous but nonblocking:

- A **16 MiB** ordinary budget charges each message's allocation **capacity** plus
  `Vec` header until its in-flight write finishes, not merely until dequeue.
- A **64 KiB reserve** supports small input/replies. Bulk admission permits at
  most 4,096 outstanding messages; nonbulk admission adds 1,024 slots. These
  limits prevent many tiny messages bypassing byte policy.
- Oversized/full/closed user input is rejected atomically and shown as a passive
  notice. Never silently discard keyboard bytes or make the notice focusable.
- Parser response bytes use `reply`: if full, the tab retains one pending parsed
  response and pauses further drain. Writer progress re-arms a wake; admission
  preserves reply headroom while waiting. Do not drop a query reply to keep
  parsing output or spin the UI while waiting for the child to read.

[`app/clipboard.rs`](../src/app/clipboard.rs) preflights source size plus framing,
then normalizes CR/LF and adds bracketed-paste markers in one byte-building pass.
A paste is **one FIFO transaction**, written in 8 KiB chunks. Keys and replies
may follow it, never interleave inside its bracket markers. Preflight cannot stop
the platform clipboard API from first allocating the original source string.

Ordinary sender drop means graceful FIFO drain. **Tab close is cancellation**:
discard queued bytes and stop the in-flight transaction. These are deliberately
different lifecycle operations.

## Decision: cancellation-aware I/O, supervised child cleanup

On Unix, each worker owns a nonblocking CLOEXEC duplicate of the master. It
waits with `poll` on that descriptor **and a shared cancellation socket**. Shutdown
makes the socket permanently readable/hung up, so both workers notice even if
one starts waiting after the other exits. An atomic flag alone cannot wake a
blocking syscall; closing another thread's descriptor is not reliable cancellation.

On `PtySession::drop`:

1. Cancel input admission and release queued allocations outside its mutex.
2. Capture the current foreground group, publish shutdown, and cancel Unix I/O.
3. Drop the output receiver, disconnecting even a reader blocked on a full send.
4. Return without joining a live worker, waiting for a child, or invoking a
   potentially sleeping process-kill helper on the UI thread.

The supervisor reaps owned children and joins **only finished** workers. It is
event-driven. Workers start through `Startup::spawn_worker`, whose drop guard
also runs on panic: it counts the worker as finished and wakes the supervisor.
The reader returning at EOF is therefore the natural-exit signal. Close requests
and finished workers both **publish under the queue lock before notifying**, so
a supervisor between its wait predicate and its wait cannot miss either one. The
supervisor uses its 25 ms timed wait only while some job needs time-based steps:
the job is closing (hangup grace and escalation), has a finished worker whose
handle is not joinable yet, or has a finished worker while its child is
unreaped. Otherwise it waits without a timer, even while shells are alive, and
an idle window gets no supervisor wakeups.

Accepted gap: a shell that exits while a background job still holds the slave
produces no EOF. It stays a zombie until its tab closes, and its tab stays open
because no Exit arrives. Exact exit events (kqueue `EVFILT_PROC`/`NOTE_EXIT`,
a Linux pidfd) would close this gap; they are not implemented.

A startup guard owns the child immediately after spawn and submits the same
cleanup if later worker creation fails. The parent's slave handle is dropped
after spawn so it cannot keep EOF from reaching the reader.

Unix close policy:

- Hang up the still-owned shell/group and a positive foreground group validated
  as belonging to that session.
- After **250 ms**, force-kill the owned child if needed; do not recursively hunt
  detached descendants or escalate every cached foreground PID.
- Reap and relinquish child ownership before forgetting the job. Never signal a
  cached PID after reaping; the OS may already have reused it.

Other backends use their native child termination on the supervisor, not this
Unix signal protocol. At application exit, `main` drops all tabs before waiting
on **one shared two-second PTY deadline**, including earlier closed sessions.
One stuck child must not serialize every other session's cleanup. Configuration
flush is a separate shutdown step, not covered by that PTY deadline.

## Verification

```sh
cargo test --locked pty::
cargo test --locked app::scheduling::
cargo test --locked app::pty_io::
```

Real Unix PTY tests need no display. They cover idle raw children, saturated
input/output, paste/key/reply ordering, hangup-resistant escalation, natural
exit/final-output ordering, and partial startup. Process-wide thread/descriptor
counts run in an isolated subprocess so parallel tests cannot corrupt baselines;
that process also asserts that one idle child causes no timed supervisor waits.
Notification races and service order/budgets have deterministic tests.
Supervisor wakeups (close requests, returning and panicking workers) have stress
tests with fake children; a test-only hook holds the lock between the idle
predicate and the wait, so a wakeup published without the lock hangs them.

The ignored scheduling probe measures request-to-parse through production queues
and scheduling helpers, **not GUI echo latency**; see [performance](performance.md).
