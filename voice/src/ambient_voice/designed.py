"""A voice designed from a description, then cloned for speed.

Qwen3-TTS can invent a voice from a plain-English description, but only its
1.7B VoiceDesign model can, and that model peaks around 5.6 GB and re-imagines
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
import shutil
import sys
import wave
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path

import numpy as np

from .engines import _mono, cache_root

DESIGN_MODEL = "mlx-community/Qwen3-TTS-12Hz-1.7B-VoiceDesign-bf16"
CLONE_MODEL = "mlx-community/Qwen3-TTS-12Hz-0.6B-Base-4bit"
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


# A voice pack is the clone model with the reference already encoded and the
# parts that only matter at load time stripped out, saved so later starts load
# just what speaking needs. Bump when the pack layout or slimming changes.
PACK_VERSION = 1
# The text embedding is a 151,936 x 2048 bf16 table (622 MB) that the upstream
# quantisation deliberately leaves alone; 8-bit halves it.
EMBED_BITS = 8


def pack_folder(model: str, reference: Reference) -> Path:
    digest = hashlib.sha256()
    digest.update(json.dumps([PACK_VERSION, EMBED_BITS, model, reference.text]).encode())
    digest.update(reference.audio.read_bytes())
    return cache_root() / "packs" / digest.hexdigest()[:16]


class _Encoded:
    """Stands in for the speech encoder once the reference is encoded.

    The clone path only checks that an encoder exists; with the reference's
    codes cached it never calls it, so its ~220 MB of weights can go.
    """

    def encode(self, audio):
        raise RuntimeError("the reference is already encoded; the encoder was released")


def _fingerprint(ref_audio, text: str):
    """The key mlx-audio caches a reference's codes under (qwen3_tts.py)."""
    return (text, (ref_audio.size, float(ref_audio.sum())))


def _slim(model) -> None:
    """Quantise the text embedding and halve the speech decoder, in place."""
    import mlx.core as mx
    from mlx import nn

    embedding = model.talker.model.text_embedding
    model.talker.model.text_embedding = nn.QuantizedEmbedding.from_embedding(
        embedding, group_size=64, bits=EMBED_BITS
    )
    # 37-51 dB SNR against fp32 on identical codes: below anything audible.
    model.speech_tokenizer.decoder.set_dtype(mx.float16)


def build_pack(model_id: str, reference: Reference, folder: Path) -> None:
    """Load the full clone model once, slim it, encode the reference, save."""
    import mlx.core as mx
    from mlx.utils import tree_flatten
    from mlx_audio.tts.utils import load_model
    from mlx_audio.utils import get_model_path, load_audio

    source = Path(get_model_path(model_id))
    model = load_model(source, lazy=True)
    _slim(model)
    ref_audio = load_audio(str(reference.audio), sample_rate=model.sample_rate)
    for _ in model.generate(
        text="Ready.", ref_audio=ref_audio, ref_text=reference.text, lang_code="english"
    ):
        pass
    codes, text_ids = model._icl_cache[_fingerprint(ref_audio, reference.text)]
    params = dict(tree_flatten(model.parameters()))
    talker = {k: v for k, v in params.items() if not k.startswith("speech_tokenizer.")}
    decoder = {
        k.removeprefix("speech_tokenizer."): v
        for k, v in params.items()
        if k.startswith("speech_tokenizer.decoder.")
    }
    tmp = folder.with_name(folder.name + ".part")
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir(parents=True)
    mx.save_safetensors(str(tmp / "talker.safetensors"), talker)
    mx.save_safetensors(str(tmp / "decoder.safetensors"), decoder)
    mx.save_safetensors(str(tmp / "reference.safetensors"), {"codes": codes, "text_ids": text_ids})
    meta = {"version": PACK_VERSION, "model": model_id, "source": str(source)}
    meta |= {"embed_bits": EMBED_BITS, "reference_text": reference.text}
    (tmp / "pack.json").write_text(json.dumps(meta, indent=2) + "\n")
    shutil.rmtree(folder, ignore_errors=True)
    tmp.rename(folder)


def load_pack(folder: Path):
    """Build the model from a pack without ever holding the full-size weights.

    This mirrors mlx-audio 0.5.8's `base_load_model` and Qwen3-TTS
    `post_load_hook`, minus the speech encoder; the version is pinned in
    pyproject.toml, and a pack that fails to load is rebuilt once.
    """
    import mlx.core as mx
    from mlx import nn
    from mlx_audio.tts.models.qwen3_tts import Model, ModelConfig
    from mlx_audio.tts.models.qwen3_tts.config import (
        Qwen3TTSTokenizerConfig,
        Qwen3TTSTokenizerDecoderConfig,
        filter_dict_for_dataclass,
    )
    from mlx_audio.tts.models.qwen3_tts.speech_tokenizer import Qwen3TTSSpeechTokenizer
    from transformers import AutoTokenizer

    meta = json.loads((folder / "pack.json").read_text())
    source = Path(meta["source"])
    config = json.loads((source / "config.json").read_text())
    config["model_path"] = str(source)
    model = Model(ModelConfig.from_dict(config))
    weights = mx.load(str(folder / "talker.safetensors"))
    bits = config["quantization"]["bits"]

    def quantised(path, module):
        if f"{path}.scales" not in weights or not hasattr(module, "to_quantized"):
            return False
        if path == "talker.model.text_embedding":
            return {"group_size": 64, "bits": meta["embed_bits"]}
        return {"group_size": config["quantization"].get("group_size", 64), "bits": bits}

    nn.quantize(model, class_predicate=quantised)
    model.load_weights(list(weights.items()), strict=False)
    mx.eval(model.parameters())
    model.eval()
    model.tokenizer = AutoTokenizer.from_pretrained(str(source))

    st_config = json.loads((source / "speech_tokenizer" / "config.json").read_text())
    decoder_config = Qwen3TTSTokenizerDecoderConfig(
        **filter_dict_for_dataclass(Qwen3TTSTokenizerDecoderConfig, st_config["decoder_config"])
    )
    tokenizer_config = Qwen3TTSTokenizerConfig(encoder_config=None, decoder_config=decoder_config)
    for k, v in st_config.items():
        if k not in ("decoder_config", "encoder_config") and hasattr(tokenizer_config, k):
            setattr(tokenizer_config, k, v)
    speech = Qwen3TTSSpeechTokenizer(tokenizer_config)
    speech.load_weights(list(mx.load(str(folder / "decoder.safetensors")).items()))
    mx.eval(speech.parameters())
    speech.eval()
    speech.encoder_model = _Encoded()
    model.load_speech_tokenizer(speech)
    model.speech_tokenizer.decoder = mx.compile(model.speech_tokenizer.decoder)
    generation = source / "generation_config.json"
    if generation.exists():
        model.load_generate_config(json.loads(generation.read_text()))
    return model


def _build(model: str, reference: Reference, folder: Path) -> None:
    import mlx.core as mx

    build_pack(model, reference, folder)
    gc.collect()
    mx.clear_cache()


class Qwen3TtsClone:
    """Qwen3-TTS 0.6B Base cloning a reference clip. Streams; one fixed voice.

    Cloning takes no delivery instruction, so a reply's emotion reaches the
    voice only through its words.
    """

    name = "qwen3-tts"

    def __init__(self, reference: Reference, model: str = CLONE_MODEL, interval: float = 0.16):
        import mlx.core as mx
        from mlx_audio.utils import load_audio

        mx.set_cache_limit(0)
        folder = pack_folder(model, reference)
        if not (folder / "pack.json").exists():
            _build(model, reference, folder)
        try:
            self.model = load_pack(folder)
        except Exception as e:  # noqa: BLE001 — a stale or damaged pack is rebuilt
            print(f"ambient-voice: rebuilding the voice pack ({e})", file=sys.stderr)
            _build(model, reference, folder)
            self.model = load_pack(folder)
        self.sample_rate = int(self.model.sample_rate)
        self.reference = reference
        self.ref_audio = load_audio(str(reference.audio), sample_rate=self.sample_rate)
        cached = mx.load(str(folder / "reference.safetensors"))
        key = _fingerprint(self.ref_audio, reference.text)
        self.model._icl_cache[key] = (cached["codes"], cached["text_ids"])
        self.interval = interval
        # One short line now, so the first reply does not pay MLX's compile.
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
