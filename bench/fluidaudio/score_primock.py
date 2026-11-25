# /// script
# dependencies = ["jiwer>=3", "whisper-normalizer>=0.0.10", "scipy", "numpy"]
# ///
"""Score PriMock57 (two speakers): WER for the shipped decode and Ultra, speaker
count and WDER per diariser on Ultra's words. Same definitions as score.py."""
import json, os, sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from score import NORM, jiwer, rttm, wder  # noqa: E402

P = Path(os.path.expanduser("~/.cache/ambient/bench/primock"))
O = Path(os.path.expanduser("~/Code/ambient-spike/bench/out-primock"))
DIAR = ["onnx", "nemo", "nemos"]

ids = sorted(p.stem for p in (P / "wav").glob("*.wav"))
refs = {m: json.load(open(P / "refs" / f"{m}.json")) for m in ids}
hyp = {
    "onnx": {m: (O / f"asr-onnx-{m}.txt").read_text() for m in ids},
    "ultra": {m: json.load(open(O / f"asr-ultra-{m}.json"))["text"] for m in ids},
}
print(f"WER (Whisper normaliser, {len(ids)} consultations pooled)")
for asr, texts in hyp.items():
    e = n = d = 0
    for m in ids:
        r = NORM(" ".join(w[3] for w in refs[m])); o = jiwer.process_words(r, NORM(texts[m]))
        e += o.substitutions + o.deletions + o.insertions; d += o.deletions; n += len(r.split())
    print(f"  {asr:6s} {e / n:.4f}  (deletions {d / n:.4f})")

print("Speakers found (2 each)")
for d in DIAR:
    counts = [len({s[2] for s in rttm(O / f"diar-{d}" / f"{m}.rttm")}) for m in ids]
    print(f"  {d:6s} exact {sum(c == 2 for c in counts)}/{len(ids)}  counts {dict(sorted({c: counts.count(c) for c in counts}.items()))}")

print("WDER on Ultra's words (pooled)")
for d in DIAR:
    bad = tot = 0
    for m in ids:
        ws = json.load(open(O / f"asr-ultra-{m}.json"))["wordTimings"]
        b, t = wder(refs[m], [(w["startTime"], w["endTime"], w["word"]) for w in ws], rttm(O / f"diar-{d}" / f"{m}.rttm"))
        bad += b; tot += t
    print(f"  ultra + {d:6s} {bad / tot:.4f}  ({int(tot)} words)")
