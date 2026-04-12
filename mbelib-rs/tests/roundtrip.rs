//! Trait and constructor invariant tests for `AmbeDecoder`.
//!
//! These tests verify properties of the decoder type itself rather than
//! its audio output: thread-safety markers, constructor equivalence, and
//! other API-level invariants that downstream consumers depend on.

use mbelib_rs::AmbeDecoder;

/// `AmbeDecoder` must implement `Send + Sync` for use in async contexts.
///
/// D-STAR voice decoding in `stargazer` and `dstar-gateway` happens inside
/// `tokio::spawn` tasks that require `Send`. The decoder carries no
/// interior mutability or thread-local state, so `Sync` is also expected.
/// This static assertion catches accidental introduction of `!Send` or
/// `!Sync` fields (e.g., `Rc`, `Cell`, raw pointers).
#[test]
fn decoder_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AmbeDecoder>();
}

/// `AmbeDecoder::default()` and `AmbeDecoder::new()` must produce decoders
/// that behave identically.
///
/// The `Default` impl delegates to `new()`, but if someone accidentally
/// changes one without the other, the outputs would silently diverge.
/// This test feeds the same frame through both and checks bit-identical
/// output, which also verifies that the initial state is the same.
#[test]
fn decoder_default_equals_new() {
    let mut from_new = AmbeDecoder::new();
    let mut from_default = AmbeDecoder::default();

    // Use the AMBE silence constant for a meaningful comparison (not
    // all-zero, which trivially produces zero output regardless of
    // initial state).
    let ambe_silence: [u8; 9] = [0x9E, 0x8D, 0x32, 0x88, 0x26, 0x1A, 0x3F, 0x61, 0xE8];

    let pcm_new = from_new.decode_frame(&ambe_silence);
    let pcm_default = from_default.decode_frame(&ambe_silence);

    assert_eq!(
        pcm_new, pcm_default,
        "AmbeDecoder::new() and AmbeDecoder::default() produced \
         different output for the same input frame"
    );
}
