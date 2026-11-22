'use strict';

class MicCapture extends AudioWorkletProcessor {
  constructor() {
    super();
    this.phase = 0;
    this.sum = 0;
    this.count = 0;
    this.frames = new Int16Array(320);
    this.index = 0;
    this.energy = 0;
  }

  process(inputs) {
    const channel = inputs[0]?.[0];
    if (!channel) return true;
    for (const sample of channel) {
      this.sum += sample;
      this.count += 1;
      this.phase += 16000;
      if (this.phase < sampleRate) continue;
      this.phase -= sampleRate;
      const value = Math.max(-1, Math.min(1, this.sum / this.count));
      this.frames[this.index++] = value * 32767;
      this.energy += value * value;
      this.count = 0;
      this.sum = 0;
      if (this.index === this.frames.length) {
        const frames = this.frames;
        this.port.postMessage({ frames, rms: Math.sqrt(this.energy / frames.length) }, [frames.buffer]);
        this.frames = new Int16Array(320);
        this.index = 0;
        this.energy = 0;
      }
    }
    return true;
  }
}
registerProcessor('jarvis-mic', MicCapture);
