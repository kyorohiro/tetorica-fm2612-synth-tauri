const test = require('node:test'), assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), vm = require('node:vm');
const source = fs.readFileSync(path.join(__dirname, '../desktop/audio-worklet.js'), 'utf8');

function processor(options = {}, inputRate = 53267) {
  let Processor;
  const packets = [], stats = [];
  vm.runInNewContext(source, {
    sampleRate: inputRate, Float32Array, Number, Math, RangeError,
    AudioWorkletProcessor: class { constructor() { this.port = {postMessage(value) {
      if (value instanceof ArrayBuffer) packets.push(new Float32Array(value).slice());
      else stats.push(value);
    }}; } },
    registerProcessor(name, constructor) { assert.equal(name, 'tetorica-synth-desktop-output'); Processor = constructor; },
  });
  const instance = new Processor({processorOptions: {frames: 512, outputSampleRate: 48000, ...options}});
  const send = message => instance.port.onmessage({data: message});
  const render = (left, right = left) => {
    const outputs = [new Float32Array(left.length).fill(1), new Float32Array(left.length).fill(1)];
    const active = instance.process([[left, right]], [outputs]);
    assert.ok(outputs.every(channel => channel.every(value => value === 0)));
    return active;
  };
  return {instance, packets, stats, send, render};
}

test('credits bound packet production and the Web Audio speaker tap is silent', () => {
  const {instance, packets, render, send} = processor({frames: 128}, 48000);
  const left = new Float32Array(128).fill(.25), right = new Float32Array(128).fill(-.75);
  for (let index = 0; index < 10; index++) render(left, right);
  assert.equal(packets.length, 0);
  send({type: 'credit', count: 100});
  for (let index = 0; index < 20; index++) render(left, right);
  assert.equal(packets.length, 4); assert.equal(instance.credits, 0);
  for (const packet of packets) {
    assert.equal(packet.length, 256);
    for (let index = 0; index < packet.length; index += 2) {
      assert.equal(packet[index], .25); assert.equal(packet[index + 1], -.75);
    }
  }
});

test('starvation discards stale audio and stop ignores delayed credits', () => {
  const {packets, stats, render, send} = processor({frames: 256}, 48000);
  const block = value => new Float32Array(128).fill(value);
  send({type: 'credit', count: 1});
  render(block(.25)); render(block(.25));
  for (let index = 0; index < 20; index++) render(block(-.75));
  send({type: 'credit', count: 1}); render(block(.5)); render(block(.5));
  assert.ok(packets[1].every(sample => sample === .5));
  send({type: 'stats'}); assert.equal(stats[0].droppedFrames, 2560);
  send({type: 'stop'}); send({type: 'credit', count: 4});
  assert.equal(render(block(1)), false); assert.equal(packets.length, 2);
});

test('53,267 Hz PCM resamples to device rate without drift or quantum discontinuities', () => {
  for (const outputSampleRate of [44100, 48000, 96000, 32000]) {
    const inputRate = 53267, inputFrames = inputRate * 2;
    const {instance, packets, render, send} = processor({frames: 128, outputSampleRate}, inputRate);
    let processed = 0;
    while (processed < inputFrames) {
      const size = Math.min(processed % 3 ? 128 : 73, inputFrames - processed);
      const left = Float32Array.from({length: size}, (_, index) => Math.sin((processed + index) * .03) * .5);
      const right = Float32Array.from(left, value => -value);
      send({type: 'credit', count: 4}); render(left, right); processed += size;
    }
    const frames = packets.length * 128 + instance.offset;
    const expectedFrames = Math.floor((inputFrames - 1) * outputSampleRate / inputRate) + 1;
    assert.equal(frames, expectedFrames);
    let frame = 0;
    for (const packet of [...packets, instance.samples.subarray(0, instance.offset * 2)]) {
      for (let index = 0; index < packet.length; index += 2, frame++) {
        const inputPosition = frame * inputRate / outputSampleRate;
        const lower = Math.floor(inputPosition), fraction = inputPosition - lower;
        const value = Math.sin(lower * .03) * .5 * (1 - fraction) + Math.sin((lower + 1) * .03) * .5 * fraction;
        assert.ok(Math.abs(packet[index] - value) < 1e-5, `${outputSampleRate} Hz, frame ${frame}`);
        assert.ok(Math.abs(packet[index + 1] + value) < 1e-5);
      }
    }
  }
});

test('mono duplicates to stereo and unsafe PCM becomes finite device-range samples', () => {
  const {instance, packets, send} = processor({frames: 5}, 48000);
  send({type: 'credit', count: 1});
  instance.process([[Float32Array.of(NaN, Infinity, -Infinity, 2, -2)]], [[new Float32Array(5)]]);
  assert.deepEqual(Array.from(packets[0]), [0, 0, 0, 0, 0, 0, 1, 1, -1, -1]);
});

test('invalid settings and malformed credit messages cannot overrun packet bounds', () => {
  for (const setting of [{frames: 0}, {frames: Infinity}, {frames: 1.5}, {maxCredits: 0}, {maxCredits: 33}, {outputSampleRate: NaN}, {outputSampleRate: 0}]) {
    assert.throws(() => processor(setting), /Invalid native audio/);
  }
  const {instance, send} = processor();
  for (const count of [NaN, Infinity, -1, .5, '4']) send({type: 'credit', count});
  assert.equal(instance.credits, 0);
});

test('independent device clocks receive slow bounded correction without resetting resampling phase', () => {
  const {instance, send, render} = processor();
  send({type: 'credit', count: 4}); render(new Float32Array(128).fill(.5));
  const phase = instance.nextOutput, base = instance.baseStep;
  send({type: 'clock', queuedFrames: 2048, intervalSeconds: .02});
  assert.ok(instance.step > base && instance.step < base * 1.0001);
  assert.equal(instance.nextOutput, phase);
  for (let index = 0; index < 1000; index++) send({type: 'clock', queuedFrames: 2048, intervalSeconds: .02});
  assert.ok(instance.step > base * 1.0019 && instance.step <= base * 1.002);
  for (let index = 0; index < 1000; index++) send({type: 'clock', queuedFrames: 0, intervalSeconds: .02});
  assert.ok(instance.step < base * .9981 && instance.step >= base * .998);
  const safeStep = instance.step;
  for (const [queuedFrames, intervalSeconds] of [[Infinity, .02], [-1, .02], [2049, .02], [1024, NaN], [1024, -1]]) {
    send({type: 'clock', queuedFrames, intervalSeconds}); assert.equal(instance.step, safeStep);
  }
});
