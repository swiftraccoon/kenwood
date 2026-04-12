//! Calling `send_voice` on a `Session<_, Configured>` must be a
//! compile error — voice TX is only available in the `Connected`
//! state.
//!
//! `send_voice` is only implemented on `Session<P, Connected>`. The
//! freshly built session is in `Configured`, so the method does not
//! resolve and the typestate keeps voice frames out of an unlinked
//! session.

use dstar_gateway_core::session::client::{DExtra, Session};
use dstar_gateway_core::types::{Callsign, Module, StreamId};
use dstar_gateway_core::voice::VoiceFrame;
use std::time::Instant;

fn main() {
    let mut session = Session::<DExtra, _>::builder()
        .callsign(Callsign::try_from_str("W1AW").unwrap())
        .local_module(Module::try_from_char('B').unwrap())
        .reflector_module(Module::try_from_char('C').unwrap())
        .peer("127.0.0.1:30001".parse().unwrap())
        .build();

    // ERROR: no method named `send_voice` found for
    // `Session<DExtra, Configured>` — `send_voice` is only
    // implemented on `Session<P, Connected>`.
    let frame = VoiceFrame::silence();
    let _ = session.send_voice(
        Instant::now(),
        StreamId::new(0x1234).unwrap(),
        0,
        &frame,
    );
}
