// SPDX-License-Identifier: GPL-2.0-or-later
//
// mbelib AMBE 3600x2400 decoder reference tool.
//
// Reads concatenated 9-byte AMBE frames from argv[1], runs mbelib's
// `mbe_processAmbe3600x2400Frame` (with internal ECC + parameter
// decode + synthesis), writes s16le PCM to argv[2], and prints a
// deterministic per-frame summary to argv[3] (or stderr).
//
// Usage: ambe_decode_dump <in.ambe> <out.s16> <out.trace>

extern "C" {
#include "mbelib.h"
}

#include <cstdio>
#include <cstdint>
#include <cstring>

// D-STAR interleave table from DSD (szechyjs/dsd), the same
// permutation our Rust `unpack::INTERLEAVE` uses. Maps input-bit
// index (the on-wire bit order produced by a real D-STAR radio's
// DVSI chip) to the flat `ambe_fr` position expected by mbelib's
// 4-codeword layout.
//
// flat[0..24]  = C0 (Golay24: 1 outer-parity + 23 Golay23 bits)
// flat[24..47] = C1 (Golay23, with LFSR scramble until mbelib descrambles)
// flat[47..58] = C2 (11 raw bits)
// flat[58..72] = C3 (14 raw bits)
//
// This is the D-STAR standard wire format — NOT OP25's P25-style
// interleave. OP25's `p25p2_vf::encode_dstar` uses a different
// permutation and a different Golay(24,12) layout (24+24+24 bits)
// which does not interoperate with real D-STAR radios.
static const int INTERLEAVE_FORWARD[72] = {
    10, 22, 69, 56, 34, 46, 11, 23, 32, 44,  9, 21,
    68, 55, 33, 45, 66, 53, 31, 43,  8, 20, 67, 54,
     6, 18, 65, 52, 30, 42,  7, 19, 28, 40,  5, 17,
    64, 51, 29, 41, 62, 49, 27, 39,  4, 16, 63, 50,
     2, 14, 61, 48, 26, 38,  3, 15, 24, 36,  1, 13,
    60, 47, 25, 37, 58, 70, 57, 35,  0, 12, 59, 71,
};

static inline void bytes_to_ambe_fr(const uint8_t bytes[9], char ambe_fr[4][24]) {
    // Unpack 9 wire bytes MSB-first → 72 input bits (same order a
    // D-STAR radio transmits them), then apply the forward
    // interleave to produce the flat `ambe_fr`-equivalent layout,
    // and finally distribute flat → mbelib's [4][24] structure.
    uint8_t wire[72];
    for (int i = 0; i < 9; i++) {
        for (int b = 0; b < 8; b++) {
            wire[i * 8 + b] = (bytes[i] >> (7 - b)) & 1;
        }
    }
    uint8_t flat[72] = {0};
    for (int i = 0; i < 72; i++) flat[INTERLEAVE_FORWARD[i]] = wire[i];

    // flat → ambe_fr[4][24] per mbelib's expected partitioning.
    for (int j = 0; j < 24; j++) ambe_fr[0][j] = flat[j];           // C0
    for (int j = 0; j < 24; j++) ambe_fr[1][j] = (j < 23) ? flat[24 + j] : 0;  // C1 (23 bits)
    for (int j = 0; j < 24; j++) ambe_fr[2][j] = (j < 11) ? flat[47 + j] : 0;  // C2 (11 bits)
    for (int j = 0; j < 24; j++) ambe_fr[3][j] = (j < 14) ? flat[58 + j] : 0;  // C3 (14 bits)
}

int main(int argc, char **argv) {
    if (argc != 4) {
        std::fprintf(stderr,
            "usage: %s <in.ambe> <out.s16> <out.trace>\n",
            argv[0]);
        return 2;
    }
    FILE *in = std::fopen(argv[1], "rb");
    if (!in) { std::perror("open in"); return 1; }
    FILE *out = std::fopen(argv[2], "wb");
    if (!out) { std::perror("open out"); return 1; }
    FILE *trace = (std::strcmp(argv[3], "-") == 0) ? stderr : std::fopen(argv[3], "w");
    if (!trace) { std::perror("open trace"); return 1; }

    mbe_parms cur_mp, prev_mp, prev_mp_enhanced;
    mbe_initMbeParms(&cur_mp, &prev_mp, &prev_mp_enhanced);

    uint8_t bytes[9];
    int frame_idx = 0;
    while (std::fread(bytes, 1, 9, in) == 9) {
        char ambe_fr[4][24];
        char ambe_d[49];
        std::memset(ambe_fr, 0, sizeof(ambe_fr));
        std::memset(ambe_d, 0, sizeof(ambe_d));
        bytes_to_ambe_fr(bytes, ambe_fr);

        int errs = 0, errs2 = 0;
        char err_str[64] = {0};
        short aout[160];
        std::memset(aout, 0, sizeof(aout));
        mbe_processAmbe3600x2400Frame(aout, &errs, &errs2, err_str, ambe_fr, ambe_d,
                                       &cur_mp, &prev_mp, &prev_mp_enhanced, 3);

        std::fwrite(aout, sizeof(short), 160, out);

        std::fprintf(trace, "FRAME %d\n", frame_idx);
        std::fprintf(trace, "  wire_bytes =");
        for (int i = 0; i < 9; i++) std::fprintf(trace, " %02x", bytes[i]);
        std::fprintf(trace, "\n");
        std::fprintf(trace, "  ambe_d =");
        for (int i = 0; i < 49; i++) std::fprintf(trace, "%d", ambe_d[i]);
        std::fprintf(trace, "\n");
        std::fprintf(trace, "  w0 = %.6f  L = %d  gamma = %.4f  errs = %d  errs2 = %d\n",
                     cur_mp.w0, cur_mp.L, cur_mp.gamma, errs, errs2);
        frame_idx++;
    }

    std::fclose(in);
    std::fclose(out);
    if (trace != stderr) std::fclose(trace);
    return 0;
}
