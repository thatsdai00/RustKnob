# Prepares the demo video's music: a stretch of a track, cut on its beat grid, plus what the video needs to move
# with it. Writes into <work folder>:
#   music.wav   the stretch, faded in and out
#   levels.csv  per video frame (30 fps): the kick (0..1), then five bands, bass .. treble (0..1)
#   beat.txt    seconds per beat (the video's timeline is written at 120 BPM and runs at the track's tempo)
#
#   python scripts/demo-music.py <track.mp3> <ffmpeg.exe> <work folder>
#
# The track: "Night Owl" by Broke For Free, CC BY 3.0 (https://archive.org/details/Directionless_EP-8295).
# 128.7 BPM; its drop at 59.576 s lands on the moment the capsule first appears (beat 8 of the video).
import subprocess, sys, wave
import numpy as np

track, ffmpeg, out = sys.argv[1:4]
SR = 44100
FPS = 30
BPM = 128.70
DROP = 59.576           # s into the track
DROP_BEAT = 8           # the video's beat it lands on
TIMELINE_BEATS = 104    # the video is 52 s at 120 BPM
beat = 60 / BPM
start = DROP - DROP_BEAT * beat
length = TIMELINE_BEATS * beat

pcm = subprocess.run([ffmpeg, "-loglevel", "error", "-ss", f"{start:.4f}", "-t", f"{length + 0.5:.4f}", "-i", track,
                      "-ac", "2", "-ar", str(SR), "-f", "s16le", "-"], capture_output=True, check=True).stdout
x = np.frombuffer(pcm, np.int16).reshape(-1, 2).astype(np.float32) / 32768
x = x[: int(length * SR)]
fade_in, fade_out = int(0.25 * SR), int(2.5 * SR)
x[:fade_in] *= np.linspace(0, 1, fade_in)[:, None]
x[-fade_out:] *= np.linspace(1, 0, fade_out)[:, None] ** 1.5
with wave.open(f"{out}/music.wav", "wb") as w:
    w.setnchannels(2); w.setsampwidth(2); w.setframerate(SR)
    w.writeframes((x * 32767).astype(np.int16).tobytes())

m = x.mean(1)
win = 2048
freqs = np.fft.rfftfreq(win, 1 / SR)
bands = [(30, 150), (150, 500), (500, 2000), (2000, 6000), (6000, 15000)]
frames = int(length * FPS)
spec = []
for f in range(frames):
    c = int(f / FPS * SR)
    seg = m[max(0, c - win // 2): c + win // 2]
    seg = np.pad(seg, (0, win - len(seg)))
    spec.append(np.abs(np.fft.rfft(seg * np.hanning(win))))
spec = np.array(spec)
rows = np.array([[np.sqrt(np.mean(s[(freqs >= a) & (freqs < b)] ** 2)) for a, b in bands] for s in spec])
rows = np.clip(rows / np.percentile(rows, 97, axis=0), 0, 1) ** 0.8
# the kick: how sharply the low end rises, as a quick-decaying pulse
low = rows[:, 0]
rise = np.maximum(0, np.diff(low, prepend=low[0]))
rise = np.clip(rise / np.percentile(rise, 98), 0, 1)
kick = np.zeros(frames)
for f in range(frames):
    kick[f] = max(rise[f], kick[f - 1] * 0.72 if f else 0)
with open(f"{out}/levels.csv", "w") as fh:
    for f in range(frames):
        fh.write(",".join(f"{v:.3f}" for v in [kick[f], *rows[f]]) + "\n")
with open(f"{out}/beat.txt", "w") as fh:
    fh.write(f"{beat:.6f}\n")
print(f"from {start:.3f} s, {length:.2f} s, {frames} frames, {beat:.5f} s per beat")
