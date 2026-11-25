# /// script
# dependencies = ["jiwer>=3", "whisper-normalizer>=0.0.10", "scipy", "numpy"]
# ///
"""Score the AMI benchmark: WER per ASR, and WDER (who said each word) per ASR x diariser.

WDER: hypothesis words get the diariser speaker that overlaps them most; words are aligned
to the manual reference (lowercased, punctuation stripped); among aligned word pairs
(correct or substituted), WDER is the share whose speaker is wrong under the best
one-to-one mapping of hypothesis to reference speakers. Lower is better.
"""
import bisect, json, os, re, sys
from collections import Counter
import jiwer, numpy as np
from scipy.optimize import linear_sum_assignment
from whisper_normalizer.english import EnglishTextNormalizer

NORM = EnglishTextNormalizer()
A = os.path.expanduser("~/.cache/ambient/bench/ami")
O = os.path.expanduser("~/Code/ambient-spike/bench/out")
M = "IS1009a IS1009b IS1009c IS1009d ES2004a ES2004b ES2004c ES2004d TS3003a TS3003b TS3003c TS3003d EN2002a EN2002b EN2002c EN2002d".split()
ASR = ["onnx", "v3", "ultra", "phonon2"]
DIAR = ["onnx", "nemo"]


def tok(s):
    return re.sub(r"[^a-z0-9' ]", " ", s.lower()).split()


def hyp_words(asr, mic, m):
    if asr == "onnx":
        return None
    d = json.load(open(f"{O}/asr-{asr}-{mic}-{m}.json"))
    return [(w["startTime"], w["endTime"], w["word"]) for w in d["wordTimings"]]


def hyp_text(asr, mic, m):
    if asr == "onnx":
        return open(f"{O}/asr-onnx-{mic}-{m}.txt").read()
    return json.load(open(f"{O}/asr-{asr}-{mic}-{m}.json"))["text"]


def rttm(path):
    segs = []
    for line in open(path):
        f = line.split()
        if f and f[0] == "SPEAKER":
            s, d = float(f[3]), float(f[4])
            segs.append((s, s + d, f[7]))
    return sorted(segs)


def speaker_at(segs, starts, s, e):
    """Speaker overlapping [s,e] most; else nearest segment by midpoint."""
    i = bisect.bisect_right(starts, e)
    best, bo = None, 0.0
    for j in range(max(0, i - 50), i):
        a, b, spk = segs[j]
        o = min(b, e) - max(a, s)
        if o > bo:
            best, bo = spk, o
    if best:
        return best
    mid = (s + e) / 2
    near = min(segs[max(0, i - 3): i + 3] or segs, key=lambda x: min(abs(x[0] - mid), abs(x[1] - mid)))
    return near[2]


def wder(ref, hyp, segs):
    starts = [x[0] for x in segs]
    rt, rs = [], []
    for _, _, spk, w in ref:
        for t in tok(w):
            rt.append(t); rs.append(spk)
    ht, hs = [], []
    for s, e, w in hyp:
        spk = speaker_at(segs, starts, s, e)
        for t in tok(w):
            ht.append(t); hs.append(spk)
    out = jiwer.process_words(" ".join(rt), " ".join(ht))
    pairs = []
    for c in out.alignments[0]:
        if c.type in ("equal", "substitute"):
            for k in range(c.ref_end_idx - c.ref_start_idx):
                pairs.append((rs[c.ref_start_idx + k], hs[c.hyp_start_idx + k]))
    R = sorted({p[0] for p in pairs}); H = sorted({p[1] for p in pairs})
    C = np.zeros((len(R), len(H)))
    for (r, h), n in Counter(pairs).items():
        C[R.index(r), H.index(h)] = n
    ri, ci = linear_sum_assignment(-C)
    return len(pairs) - C[ri, ci].sum(), len(pairs)


def main():
    mics = sys.argv[1:] or ["Mix-Headset", "Array1-01"]
    for mic in mics:
        print(f"\n=== {mic} ===")
        refs = {m: json.load(open(f"{A}/refs/{m}.json")) for m in M}
        print("WER (Whisper normaliser, 16 meetings pooled)")
        for asr in ASR:
            e = n = 0
            for m in M:
                r = NORM(" ".join(w[3] for w in refs[m])); h = NORM(hyp_text(asr, mic, m))
                o = jiwer.process_words(r, h)
                e += o.substitutions + o.deletions + o.insertions; n += len(r.split())
            print(f"  {asr:8s} {e / n:.4f}")
        print("Speakers found (ref 4 each, EN2002c 3)")
        for d in DIAR:
            counts = [len({s[2] for s in rttm(f'{O}/diar-{d}-{mic}/{m}.rttm')}) for m in M]
            ref = [len({w[2] for w in refs[m]}) for m in M]
            exact = sum(c == r for c, r in zip(counts, ref))
            print(f"  {d:8s} exact {exact}/16  abs err {sum(abs(c - r) for c, r in zip(counts, ref))}  {counts}")
        print("WDER (wrong speaker among aligned words, pooled)")
        for asr in ASR[1:]:
            for d in DIAR:
                bad = tot = 0
                for m in M:
                    b, t = wder(refs[m], hyp_words(asr, mic, m), rttm(f"{O}/diar-{d}-{mic}/{m}.rttm"))
                    bad += b; tot += t
                print(f"  {asr:8s} + {d:5s} {bad / tot:.4f}  ({int(tot)} words)")


if __name__ == "__main__":
    main()
