import { invoke } from "@tauri-apps/api/core";

export interface SoundEntry {
  name: string;
  source: string;
  mime: string;
}

export interface SoundSettings {
  needs_input: string;
  completed: string;
  volume: number;
  wait_for_subagents: boolean;
}

let list: Promise<SoundEntry[]> | null = null;

export const loadSounds = () => (list ??= invoke<SoundEntry[]>("list_sounds"));

// Web Audio rather than an HTMLMediaElement: WebKit makes any app that plays a media element the system's Now Playing
// app, which hands it the media keys.
let ctx: AudioContext | null = null;
const audio = () => (ctx ??= new AudioContext());

const buffers = new Map<string, Promise<AudioBuffer>>();

function buffer(sound: SoundEntry): Promise<AudioBuffer> {
  const key = sound.name.toLowerCase();
  let b = buffers.get(key);
  if (!b) {
    b = invoke<ArrayBuffer>("sound_data", { name: sound.name }).then((bytes) => audio().decodeAudioData(bytes));
    b.catch(() => buffers.delete(key));
    buffers.set(key, b);
  }
  return b;
}

let playing: AudioBufferSourceNode | null = null;

/// Maps the 0..1 volume setting to a gain on a cubic taper, since loudness is heard logarithmically: 0.5 is about
/// -18 dB rather than the -6 dB a linear gain gives, so the whole slider range is audible.
export const gainFor = (volume: number) => Math.min(1, Math.max(0, volume)) ** 3;

/// Plays the named sound, cutting off whatever is still playing; an empty or unknown name plays nothing.
export async function play(name: string, volume: number) {
  if (!name) return;
  const sound = (await loadSounds()).find((s) => s.name.toLowerCase() === name.toLowerCase());
  if (!sound) return;
  const data = await buffer(sound);
  const ac = audio();
  // Unconditional: a suspend from the last sound ending can still be pending while the state reads "running".
  await ac.resume();
  const gain = ac.createGain();
  gain.gain.value = gainFor(volume);
  gain.connect(ac.destination);
  const source = ac.createBufferSource();
  source.buffer = data;
  source.connect(gain);
  source.onended = () => {
    source.disconnect();
    gain.disconnect();
    if (playing !== source) return;
    playing = null;
    // A running context holds the output device open and keeps waking the audio thread.
    ac.suspend().catch(() => {});
  };
  playing?.stop();
  playing = source;
  source.start();
}
