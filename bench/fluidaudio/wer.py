# /// script
# dependencies = ["jiwer>=3", "whisper-normalizer>=0.0.10"]
# ///
"""WER for each hypothesis/reference pair, Whisper English normaliser (Open ASR leaderboard style).
usage: uv run wer.py REF HYP [REF HYP ...]   (hyp may be .txt or FluidAudio .json)"""
import json, sys, jiwer
from whisper_normalizer.english import EnglishTextNormalizer
norm = EnglishTextNormalizer()
def text(p):
    if p.endswith(".json"):
        d = json.load(open(p)); return d.get("text") or d.get("transcript") or " ".join(w.get("word","") for w in d.get("words", []))
    return open(p).read()
a = sys.argv[1:]; prefix = a[0] == "--prefix"
if prefix: a = a[1:]  # reference is the whole recording, hypothesis a leading clip
tot_e = tot_n = 0
for ref, hyp in zip(a[::2], a[1::2]):
    r, h = norm(text(ref)), norm(text(hyp))
    if prefix:  # cut the reference where it best matches the clip's end
        rw, hn = r.split(), len(h.split())
        cands = range(max(1, int(hn * .7)), min(len(rw), int(hn * 1.3)) + 1, 2)
        r = " ".join(rw[:min(cands, key=lambda k: jiwer.wer(" ".join(rw[:k]), h))])
    o = jiwer.process_words(r, h); n = len(r.split()); e = o.substitutions + o.deletions + o.insertions
    tot_e += e; tot_n += n
    print(f"{hyp.split('/')[-1]:28s} WER {e/n:.4f}  S{o.substitutions} D{o.deletions} I{o.insertions}  ref {n}w")
print(f"{'total':28s} WER {tot_e/tot_n:.4f}")
