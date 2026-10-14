// Capture the final Web Audio mix for the Rust output device. This processor's
// Web Audio output is silent; analyser/recording branches remain on the mix bus.
class SynthDesktopAudioOutput extends AudioWorkletProcessor {
  constructor(options) {
    super();
    const settings = options.processorOptions ?? {};
    this.frames = settings.frames ?? 512;
    this.maxCredits = settings.maxCredits ?? 4;
    const outputSampleRate = settings.outputSampleRate ?? sampleRate;
    if (!Number.isInteger(this.frames) || this.frames < 1 || this.frames > 65536 ||
        !Number.isInteger(this.maxCredits) || this.maxCredits < 1 || this.maxCredits > 32 ||
        !Number.isFinite(outputSampleRate) || outputSampleRate < 8000 || outputSampleRate > 384000) {
      throw new RangeError('Invalid native audio packet size, credits, or sample rate');
    }
    this.baseStep = sampleRate / outputSampleRate;
    this.step = this.baseStep;
    this.targetQueuedFrames = this.frames * this.maxCredits / 2;
    this.smoothedQueuedFrames = this.targetQueuedFrames;
    this.samples = new Float32Array(this.frames * 2);
    this.offset = 0;
    this.credits = 0;
    this.active = true;
    // Retain the fractional resampling phase and previous sample across render
    // quanta. In particular, Synth's 53,267 Hz clock is not a 48,000 Hz device.
    this.inputFrame = 0;
    this.nextOutput = 0;
    this.previousLeft = 0;
    this.previousRight = 0;
    this.droppedFrames = 0;
    this.port.onmessage = event => {
      const message = event.data;
      if (message?.type === 'credit' && this.active &&
          Number.isInteger(message.count) && message.count > 0) {
        this.credits = Math.min(this.maxCredits, this.credits + message.count);
      } else if (message?.type === 'stop') {
        this.active = false;
        this.credits = 0;
        this.offset = 0;
      } else if (message?.type === 'clock' && this.active &&
          Number.isFinite(message.queuedFrames) && message.queuedFrames >= 0 &&
          message.queuedFrames <= this.frames * this.maxCredits &&
          Number.isFinite(message.intervalSeconds) && message.intervalSeconds > 0) {
        // Web Audio and the hardware device run on independent clocks. Average
        // buffer fill over about a second, rather than following each packet's
        // sawtooth, and gently correct by at most 0.2%. Preserve sample phase.
        const smoothing = 1 - Math.exp(-Math.min(1, message.intervalSeconds));
        this.smoothedQueuedFrames += (message.queuedFrames - this.smoothedQueuedFrames) * smoothing;
        const error = (this.smoothedQueuedFrames - this.targetQueuedFrames) / this.targetQueuedFrames;
        this.step = this.baseStep * (1 + Math.max(-0.002, Math.min(0.002, error * 0.002)));
      } else if (message?.type === 'stats') {
        this.port.postMessage({type: 'stats', droppedFrames: this.droppedFrames});
      }
    };
  }

  process(inputs, outputs) {
    for (const output of outputs[0] ?? []) output.fill(0);
    if (!this.active) return false;
    const channels = inputs[0];
    if (!channels?.[0]) return true;
    const leftChannel = channels[0], rightChannel = channels[1] ?? leftChannel;
    // Credits bound all queued packets. If the UI/IPC stalls, discard audio
    // rather than building an unbounded stream of old notes to play later.
    if (!this.credits) this.offset = 0;
    for (let index = 0; index < leftChannel.length; index++) {
      const left = Number.isFinite(leftChannel[index]) ? leftChannel[index] : 0;
      const right = Number.isFinite(rightChannel[index]) ? rightChannel[index] : 0;
      while (this.nextOutput <= this.inputFrame) {
        if (this.credits) {
          const fraction = this.nextOutput - (this.inputFrame - 1);
          this.samples[this.offset * 2] = Math.max(-1, Math.min(1,
            this.previousLeft + (left - this.previousLeft) * fraction));
          this.samples[this.offset * 2 + 1] = Math.max(-1, Math.min(1,
            this.previousRight + (right - this.previousRight) * fraction));
          this.offset++;
          if (this.offset === this.frames) {
            this.credits--;
            this.port.postMessage(this.samples.buffer, [this.samples.buffer]);
            this.samples = new Float32Array(this.frames * 2);
            this.offset = 0;
          }
        } else {
          this.droppedFrames++;
        }
        this.nextOutput += this.step;
      }
      this.previousLeft = left;
      this.previousRight = right;
      this.inputFrame++;
    }
    return true;
  }
}

registerProcessor('tetorica-synth-desktop-output', SynthDesktopAudioOutput);
