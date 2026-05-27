// ── Available sounds ──

export const AVAILABLE_SOUNDS = [
  { id: "chime", label: "Chime" },
  { id: "pop", label: "Pop" },
  { id: "bell", label: "Bell" },
  { id: "success", label: "Success" },
  { id: "tweet", label: "Tweet" },
] as const;

// ── Shared completion sound config (single source of truth) ──
// Updated by Settings.tsx on successful IPC save, and by App.tsx on startup.
// All consumers read this object directly at event time (no stale refs).

export interface CompletionSoundConfig {
  enabled: boolean;
  sound: string;
}

let _config: CompletionSoundConfig = { enabled: false, sound: "chime" };

export function getCompletionSoundConfig(): CompletionSoundConfig {
  return _config;
}

export function updateCompletionSoundConfig(enabled: boolean, sound: string): void {
  _config = { enabled, sound };
}

// ── AudioContext singleton with promise-based lazy init ──
// Promise guards against concurrent creation if two play calls race.

let _audioCtxPromise: Promise<AudioContext> | null = null;

function getOrCreateAudioContext(): Promise<AudioContext> {
  if (_audioCtxPromise) return _audioCtxPromise;
  _audioCtxPromise = (async () => {
    const ctx = new AudioContext();
    // Resume if suspended (some environments impose autoplay policy)
    if (ctx.state === "suspended") {
      await ctx.resume();
    }
    return ctx;
  })();
  return _audioCtxPromise;
}

// ── Sound generators ──

function playChime(ctx: AudioContext) {
  const now = ctx.currentTime;
  [523.25, 659.25].forEach((freq, i) => {
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.type = "sine";
    osc.frequency.value = freq;
    gain.gain.setValueAtTime(0.3, now + i * 0.1);
    gain.gain.exponentialRampToValueAtTime(0.001, now + i * 0.1 + 0.15);
    osc.connect(gain).connect(ctx.destination);
    osc.start(now + i * 0.1);
    osc.stop(now + i * 0.1 + 0.15);
  });
}

function playPop(ctx: AudioContext) {
  const now = ctx.currentTime;
  const osc = ctx.createOscillator();
  const gain = ctx.createGain();
  osc.type = "sine";
  osc.frequency.value = 500;
  gain.gain.setValueAtTime(0.3, now);
  gain.gain.exponentialRampToValueAtTime(0.001, now + 0.05);
  osc.connect(gain).connect(ctx.destination);
  osc.start(now);
  osc.stop(now + 0.05);
}

function playBell(ctx: AudioContext) {
  const now = ctx.currentTime;
  [1, 2, 3].forEach((partial) => {
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.type = "sine";
    osc.frequency.value = 523.25 * partial;
    gain.gain.setValueAtTime(0.15 / partial, now);
    gain.gain.exponentialRampToValueAtTime(0.001, now + 0.4);
    osc.connect(gain).connect(ctx.destination);
    osc.start(now);
    osc.stop(now + 0.4);
  });
}

function playSuccess(ctx: AudioContext) {
  const now = ctx.currentTime;
  [523.25, 659.25, 783.99, 1046.5].forEach((freq, i) => {
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.type = "sine";
    osc.frequency.value = freq;
    gain.gain.setValueAtTime(0.25, now + i * 0.08);
    gain.gain.exponentialRampToValueAtTime(0.001, now + i * 0.08 + 0.12);
    osc.connect(gain).connect(ctx.destination);
    osc.start(now + i * 0.08);
    osc.stop(now + i * 0.08 + 0.12);
  });
}

function playTweet(ctx: AudioContext) {
  const now = ctx.currentTime;
  [0, 1].forEach((i) => {
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.type = "sine";
    osc.frequency.value = 2000;
    gain.gain.setValueAtTime(0.2, now + i * 0.08);
    gain.gain.exponentialRampToValueAtTime(0.001, now + i * 0.08 + 0.03);
    osc.connect(gain).connect(ctx.destination);
    osc.start(now + i * 0.08);
    osc.stop(now + i * 0.08 + 0.03);
  });
}

// ── Public playback function ──

export async function playCompletionSound(soundId: string): Promise<void> {
  // Guard against SSR / test environments where AudioContext is unavailable
  if (typeof AudioContext === "undefined" && typeof (window as any)?.AudioContext === "undefined") return;

  try {
    const ctx = await getOrCreateAudioContext();

    switch (soundId) {
      case "chime":   playChime(ctx);   break;
      case "pop":     playPop(ctx);     break;
      case "bell":    playBell(ctx);    break;
      case "success": playSuccess(ctx); break;
      case "tweet":   playTweet(ctx);   break;
      default:
        console.warn(`Unknown completion sound ID: "${soundId}", falling back to "chime"`);
        playChime(ctx);
        break;
    }
  } catch (e) {
    console.warn("Web Audio API unavailable", e);
  }
}
