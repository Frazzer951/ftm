#!/usr/bin/env python3
"""Synthesizes the app's built-in sounds into app/src-tauri/sounds/ (16-bit mono WAV, 22.05 kHz).

    python3 scripts/gen-sounds.py

Every tone is generated here, so the files carry no license beyond the repo's own. Clink.wav and Ding.wav in the same
directory are recorded samples (see its CREDITS.md); this script only writes the names below and leaves them alone.
"""

import math
import struct
import wave
from pathlib import Path

RATE = 22050
OUT = Path(__file__).resolve().parent.parent / "app" / "src-tauri" / "sounds"


def note(freq, length, decay, partials=((1, 1.0),), attack=0.004, bend=0.0):
    """A struck tone: sine partials (multiple, weight) under an exponential decay, with an optional pitch drop."""
    n = int(RATE * length)
    out = []
    phase = [0.0] * len(partials)
    for i in range(n):
        t = i / RATE
        f = freq * (1 - bend * min(t / length, 1))
        env = min(t / attack, 1) * math.exp(-t / decay)
        s = 0.0
        for k, (mult, weight) in enumerate(partials):
            phase[k] += 2 * math.pi * f * mult / RATE
            s += weight * math.sin(phase[k])
        out.append(s * env)
    return out


def sequence(*parts):
    """(start seconds, samples) pairs mixed into one buffer."""
    end = max(int(start * RATE) + len(s) for start, s in parts)
    out = [0.0] * end
    for start, s in parts:
        o = int(start * RATE)
        for i, v in enumerate(s):
            out[o + i] += v
    return out


def write(name, samples, peak=0.7):
    top = max(abs(v) for v in samples) or 1
    fade = int(RATE * 0.01)
    data = bytearray()
    for i, v in enumerate(samples):
        if i >= len(samples) - fade:
            v *= (len(samples) - i) / fade
        data += struct.pack("<h", int(v / top * peak * 32767))
    with wave.open(str(OUT / f"{name}.wav"), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(bytes(data))


BELL = ((1, 1.0), (2.0, 0.35), (3.01, 0.12))

OUT.mkdir(parents=True, exist_ok=True)
# Two rising bell notes: a turn finished.
write("Chime", sequence((0, note(659.3, 0.5, 0.16, BELL)), (0.12, note(987.8, 0.7, 0.22, BELL))))
# Two quick identical high notes: asking for attention.
write("Nudge", sequence((0, note(880.0, 0.18, 0.05, BELL)), (0.14, note(880.0, 0.35, 0.08, BELL))))
write("Bell", note(1318.5, 0.6, 0.14, BELL))
write("Blip", note(520.0, 0.14, 0.035, ((1, 1.0), (2, 0.2)), attack=0.002, bend=0.45))
write("Knock", sequence((0, note(190.0, 0.12, 0.025, ((1, 1.0), (2.3, 0.4)))), (0.13, note(190.0, 0.16, 0.03, ((1, 1.0), (2.3, 0.4))))))
write(
    "Arpeggio",
    sequence(
        (0, note(523.3, 0.4, 0.1, BELL)),
        (0.09, note(659.3, 0.4, 0.1, BELL)),
        (0.18, note(784.0, 0.6, 0.18, BELL)),
    ),
)
