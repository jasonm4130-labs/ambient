# /// script
# dependencies = ["numpy", "soundfile"]
# ///
"""Build the PriMock57 two-speaker fixture: one mono 16 kHz mix per consultation,
a word reference in the AMI refs format [start, end, speaker, word], and an
utterance-level RTTM.

PriMock57 records doctor and patient on separate channels of a video call, so
summing them gives the audio a call recording would hold. The transcripts are
manual but utterance-level: words get evenly spread times inside their
utterance. WDER aligns on word order, not time, so that only affects DER.

    uv run bench/fluidaudio/primock.py <primock57 data dir> <out dir>
"""
import json, re, sys
from pathlib import Path
import numpy as np, soundfile as sf

src, out = Path(sys.argv[1]), Path(sys.argv[2])
for d in ("wav", "refs", "rttm"):
    (out / d).mkdir(parents=True, exist_ok=True)

INTERVAL = re.compile(r'xmin = ([\d.]+)\s*\n\s*xmax = ([\d.]+)\s*\n\s*text = "(.*)"')


def utterances(path, spk):
    rows = []
    for a, b, text in INTERVAL.findall(path.read_text()):
        text = re.sub(r"<(UNIN|INAUDIBLE_SPEECH)/>", " ", text)
        text = re.sub(r"</?UNSURE>", "", text).replace('""', '"').strip()
        if text:
            rows.append((float(a), float(b), spk, text))
    return rows


ids = sorted({p.name.rsplit("_", 1)[0] for p in (src / "audio").glob("*_doctor.wav")})
manifest = []
for cid in ids:
    chans = []
    for role in ("doctor", "patient"):
        x, sr = sf.read(src / "audio" / f"{cid}_{role}.wav", dtype="float32")
        assert sr == 16000 and x.ndim == 1, (cid, role, sr, x.shape)
        chans.append(x)
    n = max(len(c) for c in chans)
    mix = sum(np.pad(c, (0, n - len(c))) for c in chans)
    peak = float(np.abs(mix).max())
    if peak > 0.99:
        mix *= 0.99 / peak
    sf.write(out / "wav" / f"{cid}.wav", mix, 16000, subtype="PCM_16")

    utts = sorted(utterances(src / "transcripts" / f"{cid}_doctor.TextGrid", "doctor")
                  + utterances(src / "transcripts" / f"{cid}_patient.TextGrid", "patient"))
    words = []
    for a, b, spk, text in utts:
        ws = text.split()
        step = (b - a) / len(ws)
        words += [(round(a + i * step, 3), round(a + (i + 1) * step, 3), spk, w) for i, w in enumerate(ws)]
    words.sort()
    (out / "refs" / f"{cid}.json").write_text(json.dumps(words))
    (out / "refs" / f"{cid}.txt").write_text(" ".join(w[3] for w in words) + "\n")
    (out / "rttm" / f"{cid}.rttm").write_text("".join(
        f"SPEAKER {cid} 1 {a:.3f} {b - a:.3f} <NA> <NA> {spk} <NA> <NA>\n" for a, b, spk, _ in utts))
    manifest.append({"wav": str(out / "wav" / f"{cid}.wav"), "rttm": str(out / "rttm" / f"{cid}.rttm"),
                     "meeting": cid, "seconds": n / 16000})
(out / "manifest.json").write_text(json.dumps(manifest, indent=1))
print(f"{len(manifest)} consultations, {sum(m['seconds'] for m in manifest) / 3600:.2f} h")
