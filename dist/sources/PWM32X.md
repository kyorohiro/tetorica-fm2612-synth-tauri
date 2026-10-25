# 32X PWM shared source

The asynchronous PWM factory uses this C++ core through WASM. Tauri Native builds the same core directly. The synchronous JavaScript constructor remains available for compatibility/reference.

From this sources directory, run `sh scripts/build_pwm32x_wasm.sh` with Emscripten installed. The generated loader/binary are written to `docs/generated/`. Source files retain their original relative paths.

MAME-derived FIFO/routing/timer code uses BSD-3-Clause, copyright David Haywood. See `third_party/mame-32x-pwm/LICENSE` and its README for the pinned upstream revision and host PCM differences. The original upstream source is included. No CPU, DMA or analog PWM filtering is emulated.
