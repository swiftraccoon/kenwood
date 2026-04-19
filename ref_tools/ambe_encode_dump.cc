// SPDX-License-Identifier: GPL-3.0-or-later
//
// OP25 AMBE encoder reference tool.
//
// Reads signed-16-bit little-endian 8 kHz mono PCM from argv[1] (or
// stdin if "-") in 160-sample frames, runs OP25's ambe_encoder in
// D-STAR mode, and for each frame:
//
// 1. Writes the 9 AMBE bytes to argv[2] concatenated as a raw stream.
// 2. Prints a deterministic one-frame-per-block summary to argv[3]
//    (or stderr if "-") with the encoder's internal state in a
//    format designed to be line-by-line diffable against an
//    equivalent dump from our Rust encoder.
//
// Usage:
//   ambe_encode_dump <in.s16> <out.ambe> <out.trace>

#include <cstdio>
#include <cstdint>
#include <cstdlib>
#include <cstring>

// `ambe_encoder` embeds `imbe_vocoder`, `p25p2_vf`, and `mbe_parms`
// by value — their full types must be visible at the point of its
// field layout.
#include "imbe_vocoder/imbe_vocoder.h"
#include "p25p2_vf.h"
extern "C" {
#include "mbelib.h"
}
// Expose ambe_encoder's private `vocoder` for intermediate-value dumps
// — required for bit-exact diffing against the Rust port. Diagnostic
// only; never link this object into production code.
#define private public
#include "ambe_encoder.h"
#undef private

int main(int argc, char **argv) {
    if (argc != 4) {
        std::fprintf(stderr,
            "usage: %s <in.s16> <out.ambe> <out.trace>\n"
            "  in.s16    : signed 16-bit LE PCM @ 8 kHz mono, or '-' for stdin\n"
            "  out.ambe  : 9-byte-per-frame AMBE stream\n"
            "  out.trace : human-readable intermediate dump, or '-' for stderr\n",
            argv[0]);
        return 2;
    }

    FILE *in = (std::strcmp(argv[1], "-") == 0) ? stdin : std::fopen(argv[1], "rb");
    if (!in) { std::perror("open in"); return 1; }
    FILE *out_ambe = std::fopen(argv[2], "wb");
    if (!out_ambe) { std::perror("open out.ambe"); return 1; }
    FILE *trace = (std::strcmp(argv[3], "-") == 0) ? stderr : std::fopen(argv[3], "w");
    if (!trace) { std::perror("open out.trace"); return 1; }

    ambe_encoder enc;
    enc.set_dstar_mode();

    int16_t samples[160];
    // `ambe_encoder::encode()` in D-STAR mode writes 72 bit-per-byte
    // values (via `p25p2_vf::encode_dstar`), not 9 packed bytes.  We
    // pack them MSB-first into 9 bytes below to match the on-wire
    // layout that `crate::unpack::pack_frame` / the D-STAR DSVT slot
    // carries.
    uint8_t bits[72];
    uint8_t packed[9];
    int frame_idx = 0;
    while (std::fread(samples, sizeof(int16_t), 160, in) == 160) {
        enc.encode(samples, bits);
        std::memset(packed, 0, sizeof(packed));
        for (int i = 0; i < 72; i++) {
            if (bits[i]) packed[i / 8] |= (uint8_t)(1 << (7 - (i % 8)));
        }
        std::fwrite(packed, 1, 9, out_ambe);

        // Round-trip through OP25's own decode_dstar to extract b[0..8]
        // so we can diff against our Rust quantizer's output.
        int b[9] = {0};
        p25p2_vf interleaver;
        size_t errs = interleaver.decode_dstar(bits, b, false);

        // Dump intermediate values from the last encode so our Rust
        // port can be diffed step-by-step. `vocoder.param()` holds
        // the IMBE params used by the most recent encode call.
        const IMBE_PARAM *imbe_param = enc.vocoder.param();
        std::fprintf(trace, "FRAME %d\n", frame_idx);
        std::fprintf(trace, "  wire_bytes =");
        for (int i = 0; i < 9; i++) std::fprintf(trace, " %02x", packed[i]);
        std::fprintf(trace, "\n");
        std::fprintf(trace, "  ref_pitch = %d  num_harms = %d\n",
                     imbe_param->ref_pitch, imbe_param->num_harms);
        std::fprintf(trace, "  sa[] =");
        for (int i = 0; i < imbe_param->num_harms; i++)
            std::fprintf(trace, " %d", imbe_param->sa[i]);
        std::fprintf(trace, "\n");
        std::fprintf(trace, "  v_uv_dsn[] =");
        for (int i = 0; i < imbe_param->num_harms; i++)
            std::fprintf(trace, " %d", imbe_param->v_uv_dsn[i]);
        std::fprintf(trace, "\n");
        std::fprintf(trace, "  b0..b8 = %d %d %d %d %d %d %d %d %d  (dstar_decode_errs=%zu)\n",
                     b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], errs);
        // Dump the mbe_parms prev state *after* encode — that's what
        // OP25 uses as input prediction reference for the NEXT frame.
        // Our encoder's equivalent `prev_log2_ml` must match these
        // values for the prediction-residual pipeline to stay aligned
        // with the on-air decoder that's also tracking these values.
        std::fprintf(trace, "  prev_L = %d  prev_gamma = %.6f\n",
                     enc.prev_mp.L, enc.prev_mp.gamma);
        std::fprintf(trace, "  prev_log2Ml =");
        for (int i = 0; i < 57; i++) std::fprintf(trace, " %.6f", enc.prev_mp.log2Ml[i]);
        std::fprintf(trace, "\n");
        frame_idx++;
    }

    std::fclose(in);
    std::fclose(out_ambe);
    if (trace != stderr) std::fclose(trace);
    return 0;
}
