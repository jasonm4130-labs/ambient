"""AMI manual words -> per-meeting reference: refs/<id>.json [[start,end,spk,word],...] and refs/<id>.txt"""
import glob, json, os, re, xml.etree.ElementTree as ET
A = os.path.expanduser("~/.cache/ambient/bench/ami"); O = f"{A}/refs"; os.makedirs(O, exist_ok=True)
TEST = "IS1009a IS1009b IS1009c IS1009d ES2004a ES2004b ES2004c ES2004d TS3003a TS3003b TS3003c TS3003d EN2002a EN2002b EN2002c EN2002d".split()
for m in TEST:
    words = []
    for f in sorted(glob.glob(f"{A}/ann/words/{m}.*.words.xml")):
        spk = f.split(".")[-3]
        for w in ET.parse(f).getroot():
            if w.tag != "w" or w.get("punc") == "true" or w.get("starttime") is None or not (w.text or "").strip():
                continue
            words.append([float(w.get("starttime")), float(w.get("endtime")), spk, w.text.strip()])
    words.sort()
    json.dump(words, open(f"{O}/{m}.json", "w"))
    open(f"{O}/{m}.txt", "w").write(" ".join(w[3] for w in words))
    print(m, len(words), "words", len({w[2] for w in words}), "speakers")
