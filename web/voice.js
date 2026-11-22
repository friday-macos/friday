'use strict';

const micButton = byId('mic');
const voiceOrb = byId('orb');
const voiceLevels = byId('levels');
const volumeControl = byId('voice-volume');
const volumeDisplay = byId('voice-volume-value');
const volumeKey = 'friday-playback-volume';
let conversationVoice = null;
try {
  const storedVolume = localStorage.getItem(volumeKey);
  if (storedVolume !== null && Number.isFinite(Number(storedVolume))) {
    volumeControl.value = storedVolume;
  }
} catch {}
volumeDisplay.value = volumeControl.value + '%';
volumeControl.addEventListener('input', () => {
  volumeDisplay.value = volumeControl.value + '%';
  conversationVoice?.setVolume(volumeControl.valueAsNumber / 100);
  try { localStorage.setItem(volumeKey, volumeControl.value); } catch {}
});

function drawWebVoice() {
  if (!conversationVoice) return false;
  conversationVoice.draw();
  return true;
}

function stopWebVoice() { conversationVoice?.stop(); }

class BrowserVoice {
  constructor(context) {
    this.context = context;
    this.session = null;
    this.stream = null;
    this.capture = null;
    this.dead = false;
    this.writing = Promise.resolve();
    this.mode = 'idle';
    this.captureMode = 'armed';
    this.microphoneLevel = 0;
    this.meterFrame = 0;
    this.lastMeterFrame = 0;
    this.levelHistory = Array.from(voiceLevels.children, () => 0);
    this.outputGain = context.createGain();
    this.outputGain.gain.value = volumeControl.valueAsNumber / 100;
    this.analyser = context.createAnalyser();
    this.analyser.fftSize = 256;
    this.playbackSamples = new Float32Array(this.analyser.fftSize);
    this.outputGain.connect(this.analyser).connect(context.destination);
    this.hasPlayedReply = false;
    this.sources = new Set();
    this.generation = null;
    this.cursor = 0;
    this.playTime = 0;
    this.preroll = [];
    this.frames = [];
    this.run = 0;
    this.silence = 0;
    this.length = 0;
    this.backlog = 0;
    this.lastError = null;
  }

  setVolume(volume) {
    this.outputGain.gain.setTargetAtTime(volume, this.context.currentTime, 0.015);
  }

  async begin() {
    await this.context.resume();
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true }
    });
    if (this.dead) { stream.getTracks().forEach((track) => track.stop()); return; }
    this.stream = stream;
    await this.context.audioWorklet.addModule('/mic.js');
    if (this.dead) return;
    const opened = await this.command('open');
    if (this.dead) {
      if (opened.session) call('command', {action:'voice_close', session:opened.session}).catch(() => {});
      return;
    }
    this.session = opened.session;
    for (const track of stream.getTracks()) track.addEventListener('ended', () => this.stop('마이크 연결이 끊겼어.'));
    const source = this.context.createMediaStreamSource(stream);
    this.capture = new AudioWorkletNode(this.context, 'jarvis-mic');
    const silent = this.context.createGain();
    silent.gain.value = 0;
    source.connect(this.capture).connect(silent).connect(this.context.destination);
    this.capture.port.onmessage = ({data}) => this.onFrame(data);
    this.poll();
    this.draw();
    this.meterFrame = requestAnimationFrame(this.updateMeter);
  }

  async command(operation, fields = {}) {
    const response = await call('command', {action:'voice_' + operation, session:this.session, ...fields});
    return response.json();
  }

  enqueue(operation, fields) {
    this.backlog += 1;
    if (this.backlog > 20) { this.stop('연결이 느려서 음성 대화를 멈췄어.'); return; }
    this.writing = this.writing.then(async () => {
      if (!this.dead) await this.command(operation, fields);
    }).catch((error) => {
      // Apple can finalize an utterance while the last audio packets are in flight.
      const finalized = operation !== 'listen' && error.message === '먼저 듣기를 시작해줘.';
      if (!this.dead && !finalized) this.stop(error.message);
    }).finally(() => { this.backlog -= 1; });
  }

  onFrame({frames, rms}) {
    if (this.dead) return;
    this.microphoneLevel = rms;
    const playing = this.sources.size > 0;
    const quiet = rms < (playing ? 0.032 : 0.011);
    if (this.captureMode !== 'recording') {
      if (this.captureMode === 'waiting') return;
      this.preroll.push(frames);
      if (this.preroll.length > 24) this.preroll.shift();
      this.run = quiet ? Math.max(0, this.run - 2) : this.run + 1;
      if (this.run < (playing ? 10 : 4)) return;
      this.cutPlayback();
      this.captureMode = 'recording';
      this.mode = 'listening';
      this.cutPending = true;
      this.cutGeneration = this.generation;
      this.run = 0;
      this.silence = 0;
      this.length = this.preroll.length;
      this.frames = this.preroll.splice(0);
      this.enqueue('listen');
      this.draw();
      return;
    }
    this.frames.push(frames);
    this.length += 1;
    this.silence = quiet ? this.silence + 1 : 0;
    if (this.frames.length >= 16) this.flushAudio();
    if (this.silence >= 39 || this.length >= 1450) {
      this.flushAudio();
      this.captureMode = 'waiting';
      this.mode = 'recognizing';
      this.enqueue('commit');
      this.draw();
    }
  }

  flushAudio() {
    if (!this.frames.length) return;
    let binary = '';
    for (const frame of this.frames.splice(0)) {
      for (const sample of frame) binary += String.fromCharCode(sample & 255, (sample >> 8) & 255);
    }
    this.enqueue('audio', {data:btoa(binary)});
  }

  cutPlayback() {
    for (const source of this.sources) { try { source.stop(); } catch {} }
    this.sources.clear();
    this.playTime = 0;
  }

  play(packet) {
    const binary = atob(packet.data);
    const buffer = this.context.createBuffer(1, binary.length / 2, packet.rate);
    const channel = buffer.getChannelData(0);
    for (let i = 0; i < channel.length; i++) {
      let value = binary.charCodeAt(i * 2) | binary.charCodeAt(i * 2 + 1) << 8;
      if (value >= 32768) value -= 65536;
      channel[i] = value / 32768;
    }
    const source = this.context.createBufferSource();
    source.buffer = buffer;
    source.connect(this.outputGain);
    this.sources.add(source);
    source.onended = () => {
      if (this.sources.delete(source)) this.hasPlayedReply = true;
    };
    const now = this.context.currentTime;
    if (!this.playTime) this.playTime = now + 0.11;
    else if (this.playTime < now) this.playTime = now + 0.015;
    source.start(this.playTime);
    this.playTime += buffer.duration;
  }

  updateMeter = (time) => {
    if (this.dead) return;
    if (time - this.lastMeterFrame >= 42 && voiceLevels.getClientRects().length) {
      this.lastMeterFrame = time;
      let rms = this.captureMode === 'waiting' ? 0 : this.microphoneLevel;
      if (this.sources.size) {
        this.analyser.getFloatTimeDomainData(this.playbackSamples);
        const power = this.playbackSamples.reduce((sum, sample) => sum + sample * sample, 0);
        rms = Math.sqrt(power / this.playbackSamples.length);
      }
      const level = Math.min(1, rms * 5);
      this.levelHistory.shift();
      this.levelHistory.push(level);
      voiceOrb.style.setProperty('--voice-level', level.toFixed(3));
      this.levelHistory.forEach((sample, index) => {
        voiceLevels.children[index].style.setProperty('--voice-level', sample.toFixed(3));
      });
    }
    this.meterFrame = requestAnimationFrame(this.updateMeter);
  };

  async poll() {
    if (this.dead || !this.session) return;
    try {
      const state = await this.command('status', {generation:this.generation,cursor:this.cursor});
      if (this.dead) return;
      if (this.cutPending && state.generation === this.cutGeneration) {
        setTimeout(() => this.poll(), 230);
        return;
      }
      this.cutPending = false;
      if (state.generation !== this.generation) {
        this.cutPlayback();
        this.generation = state.generation;
        this.cursor = 0;
      }
      const mode = state.mode;
      if (state.transcript) byId('live-transcript').textContent = state.transcript;
      if (this.captureMode === 'waiting' && ['working','speaking','idle'].includes(mode)) this.captureMode = 'armed';
      if (this.captureMode === 'recording' && ['working','speaking'].includes(mode)) {
        this.frames = [];
        this.captureMode = 'armed';
      }
      if (this.captureMode !== 'recording') {
        for (const packet of state.packets) {
          if (packet.seq >= this.cursor) {
            this.play(packet);
            this.cursor = packet.seq + 1;
          }
        }
      }
      if (state.error && state.error !== this.lastError) say(state.error);
      this.lastError = state.error;
      this.mode = this.captureMode === 'recording' ? 'listening'
        : this.sources.size > 0 ? 'speaking' : mode;
      this.draw();
      setTimeout(() => this.poll(), this.mode === 'speaking' || this.mode === 'working' ? 130 : 230);
    } catch (error) { if (!this.dead) this.stop(error.message); }
  }

  draw() {
    micButton.textContent = '음성 대화 끝내기';
    micButton.setAttribute('aria-pressed', 'true');
    const idle = !this.session ? '마이크 연결 중'
      : this.hasPlayedReply ? '이어 말해줘 · 마이크 켜짐' : '말해줘 · 마이크 켜짐';
    const names = {idle,listening:'듣고 있어',recognizing:'음성 인식 중',working:'답변 준비 중',speaking:'답변하는 중'};
    byId('mode').textContent = names[this.mode] || names.idle;
    voiceOrb.dataset.mode = this.mode;
    voiceOrb.dataset.meter = 'live';
    voiceLevels.dataset.meter = 'live';
  }

  stop(message) {
    if (this.dead) return;
    this.dead = true;
    cancelAnimationFrame(this.meterFrame);
    delete voiceOrb.dataset.meter;
    delete voiceLevels.dataset.meter;
    voiceOrb.style.removeProperty('--voice-level');
    for (const bar of voiceLevels.children) bar.style.removeProperty('--voice-level');
    this.stream?.getTracks().forEach((track) => track.stop());
    if (this.capture) { this.capture.port.onmessage = null; this.capture.disconnect(); }
    this.cutPlayback();
    this.context.close().catch(() => {});
    if (this.session) this.command('close').catch(() => {});
    if (conversationVoice === this) conversationVoice = null;
    micButton.textContent = '음성 대화 시작';
    micButton.setAttribute('aria-pressed', 'false');
    byId('live-transcript').textContent = '';
    draw();
    if (message) say(message);
  }
}

micButton.addEventListener('click', () => {
  if (conversationVoice) { conversationVoice.stop(); return; }
  if (!window.isSecureContext || !navigator.mediaDevices?.getUserMedia || !window.AudioContext) {
    say('마이크를 쓰려면 Tailscale HTTPS 주소와 음성 입력이 가능한 브라우저가 필요해.');
    return;
  }
  const voice = new BrowserVoice(new AudioContext());
  conversationVoice = voice;
  voice.draw();
  voice.begin().catch((error) => { if (!voice.dead) voice.stop(error.message); });
});
window.addEventListener('pagehide', () => conversationVoice?.stop());
document.addEventListener('visibilitychange', () => { if (document.hidden) stopWebVoice(); });
byId('forget').addEventListener('click', () => conversationVoice?.stop(), true);
