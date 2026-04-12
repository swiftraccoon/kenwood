# 0002 — Full typestate for the client session

- **Status**: accepted
- **Date**: 2026-04-10
- **Deciders**: swiftraccoon

## Context

A D-STAR reflector client session goes through a well-defined set
of states. For DExtra and DCS:
`Configured → Connecting → Connected → Disconnecting → Disconnected`.
For DPlus, there is an additional `Authenticated` state between
`Configured` and `Connecting` because DPlus requires a TCP auth
step before the UDP LINK can begin.

The previous iteration of the library represented this with a
single struct and a runtime `State` enum:

```rust
pub struct ReflectorClient {
    state: State,
    // ...
}

impl ReflectorClient {
    pub async fn send_voice(&mut self, /* ... */) -> Result<(), Error> {
        if self.state != State::Connected {
            return Err(Error::WrongState);
        }
        // ...
    }
}
```

The failure mode was that `send_voice` compiled even when the
client was obviously disconnected. The check happened at runtime
and the error was only caught by the test that happened to exercise
that path. A new contributor adding a new method had to remember
to re-add the runtime check.

We considered three alternatives when designing the rewrite.

## Decision

The client session uses a full typestate: `Session<P, S>` is
parameterized over a protocol marker `P` and a state marker `S`.
Each state is a distinct zero-sized type that implements the
`ClientState` sealed trait. Methods are defined only on the
specific `Session<P, State>` impls where they are legal.

`send_voice` is defined only on `impl<P> Session<P, Connected>` —
so calling it on a `Session<DExtra, Configured>` is a compile-time
type error, not a runtime check.

State transitions are one-way except for `Failed<S, E>`, a wrapper
state that lets the caller inspect the last valid state and the
error before deciding whether to retry or give up.

## Consequences

### Positive

- Invalid state transitions are compile errors. The compile-fail
  tests in `dstar-gateway-core/tests/compile_fail/` are a living
  contract: they assert that *specific* invalid transitions remain
  uncompilable even as the library evolves.
- The `ClientState` sealed trait prevents downstream crates from
  adding their own states. This is important because the state set
  is a closed universe — there are no extension points for
  custom states, and opening the set would invalidate the
  compile-fail guarantees.
- IDE autocomplete becomes a documentation tool: only the methods
  that are legal in the current state appear, which teaches the
  protocol shape by discovery.
- Refactoring is safer: renaming a method or moving it to a
  different state is rejected at compile time if any caller tries
  to invoke it from the wrong state.

### Negative

- **Type-level gymnastics.** Generic code that needs to hold a
  session regardless of state (e.g. a UI that stores the current
  session in a struct field) cannot use `Session<P, S>` directly.
  The `AnySession<P>` enum addresses this by wrapping every
  possible state and providing a `state_kind()` discriminator.
- **Error recovery is more involved.** The `Failed<S, E>` wrapper
  pattern adds one generic parameter to every public method that
  can fail with recoverable state. Less ergonomic than a single
  `ReflectorClient::is_failed()` check, but also less lossy.
- **Documentation overhead.** Each transition needs its own
  rustdoc section explaining what state it lands in. This is
  incremental work but it accumulates.

## Alternatives considered

- **Single struct + runtime enum** (status quo in the old crate).
  Rejected because runtime state checks are the class of bug we
  wanted to eliminate.
- **Builder pattern with state type-tag only in the builder, and
  a single `Session` type at steady state**. Considered and
  rejected because it moves the safety guarantee from the type
  system to the programmer — after the builder hands off, the
  runtime check returns.
- **`enum Session { Configured(...), Connected(...), ... }`**.
  Considered, but the caller ends up writing a match at every
  call site and the compile-fail guarantee is lost.

## References

- `dstar-gateway-core/src/session/client/session.rs` — the
  concrete `Session<P, S>` typestate wrapper and every
  state-gated method.
- `dstar-gateway-core/tests/compile_fail/` for the living
  contract that invalid transitions stay uncompilable.
- Rust typestate blog posts: Yoshua Wuyts ("Typestate in Rust"),
  Cliff Biffle ("The Typestate Pattern").
