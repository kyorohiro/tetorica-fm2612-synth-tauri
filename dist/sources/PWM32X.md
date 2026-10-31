# 32X PWM shared source

The asynchronous PWM factory uses this C++ core through WASM. Tauri Native builds the same core directly. The synchronous JavaScript constructor remains available for compatibility/reference.

From this sources directory, run `sh scripts/build_pwm32x_wasm.sh docs/generated` with Emscripten installed. The explicit output argument writes the loader/binary to `docs/generated/`; the recipe default is `dist/wasm/`. The standalone recipe, core, wrapper and provenance come from the pinned SDK source release; no SDK checkout, Python importer or network access is needed to build this bundle. MAME provenance files are retained under `third_party/mame-32x-pwm/`.

MAME-derived FIFO/routing/timer code uses BSD-3-Clause, copyright David Haywood. See `third_party/mame-32x-pwm/LICENSE` and its README for the pinned upstream revision and host PCM differences. The original upstream source is included. No CPU, DMA or analog PWM filtering is emulated.
