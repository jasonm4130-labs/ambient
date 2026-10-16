"""The three local voices, behind one interface.

Each engine turns a line of text into float32 mono chunks at its own sample
rate. Qwen3-TTS streams natively; the two ONNX engines do not, so their lines
are split into sentences and each sentence is a chunk, which puts the first
sentence on the speaker while the rest is still being generated.

Engine modules are imported inside each constructor: a machine installs only
the extra for the engine it runs, and importing `mlx_audio` on a Kokoro
machine would fail before the helper said a word.
"""

from __future__ import annotations

import os
import re
import shutil
import urllib.request
from collections.abc import Iterator
from pathlib import Path
from typing import Protocol

import numpy as np

ENGINES = ("qwen3-tts", "supertonic3", "kokoro")

# The emotions the reply model may pick from. Qwen3-TTS takes a free-text
# instruction; these are the instructions, so the reply model chooses a mood
# rather than writing prose for a TTS model it does not know.
QWEN_INSTRUCTIONS = {
    "neutral": "Calm, natural and conversational.",
    "warm": "Warm and friendly, like a helpful colleague.",
    "amused": "Warm and lightly amused, like a helpful colleague.",
    "excited": "Upbeat and bright, with energy.",
    "apologetic": "Gentle and apologetic, a little slower.",
    "concerned": "Serious and concerned, at a measured pace.",
}

# Supertonic renders a few non-speech tags. Only two moods earn one: a laugh
# on every warm line would be grating.
SUPERTONIC_TAGS = {"amused": "<laugh>", "apologetic": "<sigh>"}

_SENTENCE = re.compile(r"(?<=[.!?])\s+")


def sentences(text: str) -> list[str]:
    """Split a reply into sentences, keeping punctuation and dropping blanks."""
    return [s.strip() for s in _SENTENCE.split(text.strip()) if s.strip()]


def cache_root() -> Path:
    """Where downloaded model files live, outside the repository and the app."""
    override = os.environ.get("AMBIENT_VOICE_CACHE")
    if override:
        return Path(override)
    return Path.home() / "Library" / "Caches" / "Ambient" / "voice"


class Engine(Protocol):
    name: str
    sample_rate: int

    def stream(self, text: str, emotion: str) -> Iterator[np.ndarray]: ...


def _mono(audio) -> np.ndarray:
    return np.asarray(audio, dtype=np.float32).reshape(-1)


class Qwen3Tts:
    """Qwen3-TTS 0.6B CustomVoice, 8-bit, on MLX. Streams; takes instructions."""

    name = "qwen3-tts"
    MODEL = "mlx-community/Qwen3-TTS-12Hz-0.6B-CustomVoice-8bit"

    def __init__(self, voice: str | None, style: str | None):
        import mlx.core as mx
        from mlx_audio.tts.utils import load_model

        # MLX's buffer cache otherwise holds freed activations, which the
        # voice survey measured inflating the footprint by gigabytes.
        mx.set_cache_limit(0)
        self.model = load_model(self.MODEL)
        self.sample_rate = int(self.model.sample_rate)
        self.voice = voice or "Vivian"
        self.style = style

    def instruction(self, emotion: str) -> str:
        mood = QWEN_INSTRUCTIONS.get(emotion, QWEN_INSTRUCTIONS["neutral"])
        return f"{self.style} {mood}" if self.style else mood

    def stream(self, text: str, emotion: str) -> Iterator[np.ndarray]:
        for result in self.model.generate(
            text=text,
            voice=self.voice,
            instruct=self.instruction(emotion),
            lang_code="english",
            stream=True,
            streaming_interval=0.5,
        ):
            yield _mono(result.audio)


class Supertonic3:
    """Supertonic 3 on ONNX Runtime. Not streamed; expression tags render."""

    name = "supertonic3"

    def __init__(self, voice: str | None, style: str | None):
        from supertonic import TTS

        self.tts = TTS(model="supertonic-3")
        self.sample_rate = int(self.tts.sample_rate)
        self.voice = self.tts.get_voice_style(voice or "F1")

    def stream(self, text: str, emotion: str) -> Iterator[np.ndarray]:
        tag = SUPERTONIC_TAGS.get(emotion)
        for i, sentence in enumerate(sentences(text)):
            line = f"{tag} {sentence}" if tag and i == 0 else sentence
            wav, _ = self.tts.synthesize(line, voice_style=self.voice, lang="en")
            yield _mono(wav)


class Kokoro:
    """Kokoro-82M int8 on ONNX Runtime: the smallest voice, with no emotion."""

    name = "kokoro"
    RELEASE = "https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0"
    FILES = ("kokoro-v1.0.int8.onnx", "voices-v1.0.bin")

    def __init__(self, voice: str | None, style: str | None):
        from kokoro_onnx import EspeakConfig
        from kokoro_onnx import Kokoro as Model

        folder = cache_root() / "kokoro"
        folder.mkdir(parents=True, exist_ok=True)
        for name in self.FILES:
            target = folder / name
            if not target.exists():
                partial = target.with_suffix(target.suffix + ".part")
                urllib.request.urlretrieve(f"{self.RELEASE}/{name}", partial)
                partial.rename(target)
        self.model = Model(
            str(folder / self.FILES[0]),
            str(folder / self.FILES[1]),
            espeak_config=EspeakConfig(data_path=str(_short_espeak_data())),
        )
        self.sample_rate = 24_000
        self.voice = voice or "af_heart"

    def stream(self, text: str, emotion: str) -> Iterator[np.ndarray]:
        for sentence in sentences(text):
            audio, _ = self.model.create(sentence, voice=self.voice, lang="en-us")
            yield _mono(audio)


def _short_espeak_data() -> Path:
    """espeak-ng aborts on the wheel's long data path, so copy it somewhere short."""
    import espeakng_loader

    target = cache_root() / "espeak-ng-data"
    if not target.exists():
        shutil.copytree(espeakng_loader.get_data_path(), target)
    return target


def make_engine(
    name: str,
    voice: str | None = None,
    style: str | None = None,
    reference: Path | None = None,
    description: str | None = None,
    cue: str | None = None,
) -> Engine:
    """Load the named engine. Raises ValueError for a name that is not one.

    For Qwen3-TTS, a reference folder or a voice description selects a
    designed voice cloned by the 0.6B Base model; otherwise it is a preset
    speaker that follows a per-reply delivery instruction.
    """
    if name == "qwen3-tts" and (reference or description):
        from .designed import Qwen3TtsClone, designed_reference, load_reference

        if reference:
            return Qwen3TtsClone(load_reference(reference))
        return Qwen3TtsClone(designed_reference(description or "", cue))
    if name == "qwen3-tts":
        return Qwen3Tts(voice, style)
    if name == "supertonic3":
        return Supertonic3(voice, style)
    if name == "kokoro":
        return Kokoro(voice, style)
    raise ValueError(f"unknown engine {name!r}; expected one of {', '.join(ENGINES)}")
