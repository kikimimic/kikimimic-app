#!/usr/bin/env python3
"""Summarize existing RTTM without downloading or running a model. Not DER."""
import argparse
import hashlib
import json
import math
import wave
from pathlib import Path

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('audio', type=Path)
p.add_argument('rttm', type=Path)
p.add_argument('--elapsed-seconds', type=float, required=True)
a = p.parse_args()
if not math.isfinite(a.elapsed_seconds) or a.elapsed_seconds <= 0:
    p.error('elapsed seconds must be finite and positive')
with wave.open(str(a.audio)) as w:
    duration = w.getnframes() / w.getframerate()
    if w.getframerate() != 16000 or w.getnchannels() != 1 or duration <= 0:
        p.error('expected nonempty 16 kHz mono WAV')
turns = []
for n, line in enumerate(a.rttm.read_text().splitlines(), 1):
    if not line.strip() or line.startswith('#'):
        continue
    fields = line.split()
    if len(fields) != 10 or fields[0] != 'SPEAKER':
        p.error(f'invalid RTTM row {n}')
    start, length = map(float, fields[3:5])
    if not all(map(math.isfinite, (start, length))) or start < 0 or length <= 0 or start + length > duration + .01:
        p.error(f'out of range RTTM row {n}')
    turns.append((start, length, fields[7]))
if not turns:
    p.error('no speaker turns')
print(json.dumps(dict(audio_sha256=hashlib.sha256(a.audio.read_bytes()).hexdigest(),
    rttm_sha256=hashlib.sha256(a.rttm.read_bytes()).hexdigest(), duration_seconds=duration,
    elapsed_seconds=a.elapsed_seconds, real_time_factor=a.elapsed_seconds / duration,
    turns=len(turns), speakers=sorted({t[2] for t in turns}),
    summed_turn_seconds=sum(t[1] for t in turns),
    accuracy='unmeasured: requires independently annotated reference'), indent=2))
