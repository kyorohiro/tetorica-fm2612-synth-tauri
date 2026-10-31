# Tetorica Sega PSG source

This original Tetorica C++ implementation uses BSD-3-Clause, copyright 2026 kyorohiro. See third_party/tetorica_sega_psg/LICENSE and README.md for the notice and credit to Maxim and the SMS Power! SN76489 documentation contributors.

From this sources directory, run `sh scripts/build_segapsg_wasm.sh ../generated` with Emscripten installed. This standalone recipe requires no SDK checkout, importer or network access. Its default output is dist/wasm; an explicit output argument selects another directory.

The wrapper, shared serialization helper and compiler recipe are verified against the fixed SDK release 0.1.7. The separated core's complete body is also checked against that release; only the explicit copyright/SPDX comment may differ. The recipe's source and include paths are relocated for this self-contained bundle, while compiler flags and exports are retained. This does not change the application's fixed SDK pin or its prebuilt runtime.

The source is in third_party/tetorica_sega_psg, its shared Tetorica helper is src/chip_state.h, and its bridge is wasm/segapsg_wasm.cpp. Sega PSG is separate from the upstream ymfm implementation.
