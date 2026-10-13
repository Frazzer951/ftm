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
}

/// Each listed type and the spellings WebKit may recognize it by; the first one `canPlayType` accepts labels the blob.
const ALIASES: Record<string, string[]> = {
  "audio/wav": ["audio/wav", "audio/x-wav", "audio/wave"],
  "audio/aiff": ["audio/aiff", "audio/x-aiff"],
  "audio/ogg": ['audio/ogg; codecs="vorbis"', "audio/ogg"],
  "audio/x-caf": ["audio/x-caf"],
};

const probe = document.createElement("audio");
const playableType = (mime: string) => (ALIASES[mime] ?? [mime]).find((m) => probe.canPlayType(m) !== "");

let list: Promise<SoundEntry[]> | null = null;

/// Sounds this webview can play (WKWebView has no Ogg Vorbis; webkitgtk's formats depend on the GStreamer plugins).
export const loadSounds = () =>
  (list ??= invoke<SoundEntry[]>("list_sounds").then((all) => all.filter((s) => playableType(s.mime))));

const urls = new Map<string, Promise<string>>();

function url(sound: SoundEntry): Promise<string> {
  const key = sound.name.toLowerCase();
  let u = urls.get(key);
  if (!u) {
    u = invoke<ArrayBuffer>("sound_data", { name: sound.name }).then((bytes) =>
      URL.createObjectURL(new Blob([bytes], { type: playableType(sound.mime) ?? sound.mime })),
    );
    u.catch(() => urls.delete(key));
    urls.set(key, u);
  }
  return u;
}

let playing: HTMLAudioElement | null = null;

/// Plays the named sound, cutting off whatever is still playing; an empty or unknown name plays nothing.
export async function play(name: string, volume: number) {
  if (!name) return;
  const sound = (await loadSounds()).find((s) => s.name.toLowerCase() === name.toLowerCase());
  if (!sound) return;
  const audio = new Audio(await url(sound));
  audio.volume = Math.min(1, Math.max(0, volume));
  playing?.pause();
  playing = audio;
  await audio.play();
}
