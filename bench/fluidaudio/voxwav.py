# /// script
# dependencies = ["pyarrow", "soundfile", "numpy"]
# ///
"""Extract VoxConverse test files with 1-3 reference speakers from the HF parquet mirror to 16 kHz wav."""
import glob, io, os, sys
import pyarrow.parquet as pq, soundfile as sf
V = os.path.expanduser("~/.cache/ambient/bench/vox"); O = f"{V}/wav"; os.makedirs(O, exist_ok=True)
want = {}
for f in glob.glob(f"{V}/rttm_test/*.rttm"):
    n = len({l.split()[7] for l in open(f) if l.strip()})
    if n <= 3: want[os.path.basename(f)[:-5]] = n
shards = sorted(glob.glob(f"{V}/hf-mirror-test/**/*.parquet", recursive=True))
print("schema:", pq.read_schema(shards[0]))
got = 0
for s in shards:
    t = pq.read_table(s)
    for row in t.to_pylist():
        a = row.get("audio") or {}
        name = os.path.basename(a.get("path") or "")[:-4] or row.get("id") or row.get("file")
        if name in want and a.get("bytes"):
            data, sr = sf.read(io.BytesIO(a["bytes"]))
            sf.write(f"{O}/{name}.wav", data, sr, subtype="PCM_16"); got += 1
print("extracted", got, "of", len(want))
