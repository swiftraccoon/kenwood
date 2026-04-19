cmake_minimum_required(VERSION 3.5)
project(ambe_reference CXX C)

set(CMAKE_CXX_STANDARD 14)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# ---- imbe_vocoder + ambe_encoder from OP25 ----
set(OP25_LIB ${CMAKE_SOURCE_DIR}/../ref/op25/op25/gr-op25_repeater/lib)

set(IMBE_SOURCES
    ${OP25_LIB}/imbe_vocoder/aux_sub.cc
    ${OP25_LIB}/imbe_vocoder/basicop2.cc
    ${OP25_LIB}/imbe_vocoder/ch_decode.cc
    ${OP25_LIB}/imbe_vocoder/ch_encode.cc
    ${OP25_LIB}/imbe_vocoder/dc_rmv.cc
    ${OP25_LIB}/imbe_vocoder/decode.cc
    ${OP25_LIB}/imbe_vocoder/dsp_sub.cc
    ${OP25_LIB}/imbe_vocoder/encode.cc
    ${OP25_LIB}/imbe_vocoder/imbe_vocoder.cc
    ${OP25_LIB}/imbe_vocoder/math_sub.cc
    ${OP25_LIB}/imbe_vocoder/pe_lpf.cc
    ${OP25_LIB}/imbe_vocoder/pitch_est.cc
    ${OP25_LIB}/imbe_vocoder/pitch_ref.cc
    ${OP25_LIB}/imbe_vocoder/qnt_sub.cc
    ${OP25_LIB}/imbe_vocoder/rand_gen.cc
    ${OP25_LIB}/imbe_vocoder/sa_decode.cc
    ${OP25_LIB}/imbe_vocoder/sa_encode.cc
    ${OP25_LIB}/imbe_vocoder/sa_enh.cc
    ${OP25_LIB}/imbe_vocoder/tbls.cc
    ${OP25_LIB}/imbe_vocoder/uv_synt.cc
    ${OP25_LIB}/imbe_vocoder/v_synt.cc
    ${OP25_LIB}/imbe_vocoder/v_uv_det.cc
)

set(AMBE_SOURCES
    ${OP25_LIB}/ambe_encoder.cc
    ${OP25_LIB}/p25p2_vf.cc
    ${OP25_LIB}/rs.cc
    ${OP25_LIB}/ambe.c
    ${OP25_LIB}/mbelib.c
)

add_library(imbe_vocoder STATIC ${IMBE_SOURCES})
target_include_directories(imbe_vocoder PUBLIC ${OP25_LIB}/imbe_vocoder)
target_compile_options(imbe_vocoder PRIVATE -fPIC -Wno-deprecated-register -Wno-register)

add_library(ambe_ref STATIC ${AMBE_SOURCES})
target_include_directories(ambe_ref PUBLIC ${OP25_LIB} ${OP25_LIB}/imbe_vocoder)
target_compile_options(ambe_ref PRIVATE -fPIC -Wno-deprecated-register -Wno-register)
target_link_libraries(ambe_ref PUBLIC imbe_vocoder)

# Link mbelib for the decoder side (already built separately).
set(MBELIB_DIR ${CMAKE_SOURCE_DIR}/../ref/mbelib)
set(MBELIB_BUILD ${MBELIB_DIR}/build)

add_library(mbelib STATIC IMPORTED)
set_target_properties(mbelib PROPERTIES
    IMPORTED_LOCATION ${MBELIB_BUILD}/libmbe.a)

# ---- Reference tools ----

# Instrumented AMBE encoder: reads s16le PCM (8 kHz mono) from stdin,
# writes 9-byte AMBE frames to a file, and prints intermediates.
add_executable(ambe_encode_dump ambe_encode_dump.cc)
target_include_directories(ambe_encode_dump PRIVATE ${OP25_LIB} ${MBELIB_DIR})
target_link_libraries(ambe_encode_dump PRIVATE ambe_ref imbe_vocoder mbelib)

# Instrumented AMBE decoder: reads 9-byte AMBE frames from a file,
# writes s16le PCM, and prints intermediates.
add_executable(ambe_decode_dump ambe_decode_dump.cc)
target_include_directories(ambe_decode_dump PRIVATE ${MBELIB_DIR})
target_link_libraries(ambe_decode_dump PRIVATE mbelib)
