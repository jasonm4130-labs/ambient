"""A voice designed from a description, then cloned for speed.

Qwen3-TTS can invent a voice from a plain-English description, but only its
1.7B VoiceDesign model can, and that model peaks around 5.3 GB and re-imagines
the voice on every call. So the description is rendered once, into a short
reference clip, and every reply is spoken by the 0.6B Base model cloning that
clip: the same voice every time, at a fraction of the memory.

A designed voice is a folder holding `reference.wav` and `voice.json` (the
reference's transcript, plus the description and cue it came from). Designs
made on demand are cached under the voice cache, keyed by everything that
shaped them, so changing the description makes a new one.
"""

from __future__ import annotations

import gc
import hashlib
import json
import wave
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from .engines import _mono, cache_root

DESIGN_MODEL = "mlx-community/Qwen3-TTS-12Hz-1.7B-VoiceDesign-bf16"
CLONE_MODEL = "mlx-community/Qwen3-TTS-12Hz-0.6B-Base-8bit"
# What the designed voice says in its reference clip. One sentence of ordinary
# meeting speech is enough to clone from; longer references cost prefill time.
REFERENCE_TEXT = "Well, that's the third meeting this week about having fewer meetings."
SEED = 1234


@dataclass(frozen=True)
class Reference:
    audio: Path
    text: str


def load_reference(folder: Path) -> Reference:
    meta = json.loads((folder / "voice.json").read_text())
    return Reference(audio=folder / meta.get("audio", "reference.wav"), text=meta["text"])


def design_key(description: str, cue: str | None, text: str) -> str:
    shaped_by = json.dumps([DESIGN_MODEL, description, cue or "", text, SEED])
    return hashlib.sha256(shaped_by.encode()).hexdigest()[:16]


def design(description: str, cue: str | None, out: Path, text: str = REFERENCE_TEXT) -> Reference:
    """Render the reference clip with VoiceDesign, then free that model."""
    import mlx.core as mx
    from mlx_audio.tts.utils import load_model

    mx.set_cache_limit(0)
    model = load_model(DESIGN_MODEL)
    mx.random.seed(SEED)
    instruct = f"{description} {cue}" if cue else description
    chunks = [
        _mono(r.audio)
        for r in model.generate(text=text, instruct=instruct, lang_code="english", stream=False)
    ]
    sample_rate = int(model.sample_rate)
    del model
    gc.collect()
    mx.clear_cache()
    out.mkdir(parents=True, exist_ok=True)
    write_wav(out / "reference.wav", np.concatenate(chunks), sample_rate)
    meta = {"text": text, "audio": "reference.wav", "description": description, "cue": cue}
    meta |= {"model": DESIGN_MODEL, "seed": SEED}
    (out / "voice.json").write_text(json.dumps(meta, indent=2) + "\n")
    return load_reference(out)


def designed_reference(description: str, cue: str | None) -> Reference:
    """The cached design for this description, designing it on first use."""
    folder = cache_root() / "designed" / design_key(description, cue, REFERENCE_TEXT)
    if (folder / "voice.json").exists():
        return load_reference(folder)
    return design(description, cue, folder)


def write_wav(path: Path, audio: np.ndarray, sample_rate: int) -> None:
    pcm = (np.clip(audio, -1.0, 1.0) * 32767).astype("<i2")
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        w.writeframes(pcm.tobytes())


class Qwen3TtsClone:
    """Qwen3-TTS 0.6B Base cloning a reference clip. Streams; one fixed voice.

    Cloning takes no delivery instruction, so a reply's emotion reaches the
    voice only through its words.
    """

    name = "qwen3-tts"

    def __init__(self, reference: Reference, model: str = CLONE_MODEL, interval: float = 0.32):
        import mlx.core as mx
        from mlx_audio.tts.utils import load_model
        from mlx_audio.utils import load_audio

        mx.set_cache_limit(0)
        self.model = load_model(model)
        self.sample_rate = int(self.model.sample_rate)
        self.reference = reference
        self.ref_audio = load_audio(str(reference.audio), sample_rate=self.sample_rate)
        self.interval = interval
        # Encode the reference now (the model caches the codes), so the first
        # reply pays for neither the encoding nor MLX's first-call compile.
        for _ in self.stream("Ready.", "neutral"):
            pass

    def stream(self, text: str, emotion: str) -> Iterator[np.ndarray]:
        for result in self.model.generate(
            text=text,
            ref_audio=self.ref_audio,
            ref_text=self.reference.text,
            lang_code="english",
            stream=True,
            streaming_interval=self.interval,
        ):
            yield _mono(result.audio)
