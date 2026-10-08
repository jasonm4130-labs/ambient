"""`ambient-voice`: the live assistant's voice, as a child process.

The protocol is newline-delimited JSON, one message per line in each
direction, so the parent (`ambient assist`) needs no audio code and a crash
here never touches the recorder.

In, on stdin:

    {"op": "say", "id": 1, "text": "...", "emotion": "warm"}
    {"op": "quit"}                       (EOF means the same)

Out, on stdout — nothing else is ever printed there:

    {"event": "ready", "engine": "qwen3-tts", "sample_rate": 24000, "load_ms": 2410}
    {"event": "speaking", "id": 1, "first_audio_ms": 91}
    {"event": "done", "id": 1, "audio_s": 3.2, "total_ms": 3350}
    {"event": "error", "id": 1, "message": "..."}

Diagnostics go to stderr.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
from collections.abc import Callable
from pathlib import Path
from typing import IO

from .engines import ENGINES, Engine, make_engine
from .player import Player, Silent, Speakers, save_wav


def emit(out: IO[str], event: dict) -> None:
    out.write(json.dumps(event) + "\n")
    out.flush()


def say(engine: Engine, player: Player, request: dict, out: IO[str], save_dir: Path | None) -> None:
    rid = request.get("id")
    text = str(request.get("text") or "").strip()
    if not text:
        emit(out, {"event": "error", "id": rid, "message": "nothing to say"})
        return
    emotion = str(request.get("emotion") or "neutral")
    started = time.perf_counter()
    chunks = []
    try:
        for chunk in engine.stream(text, emotion):
            if not chunks:
                first = round((time.perf_counter() - started) * 1000)
                emit(out, {"event": "speaking", "id": rid, "first_audio_ms": first})
            chunks.append(chunk)
            player.play(chunk)
        player.wait()
    except Exception as e:  # noqa: BLE001 — any engine failure is reported, not fatal
        emit(out, {"event": "error", "id": rid, "message": f"{type(e).__name__}: {e}"})
        return
    samples = sum(len(c) for c in chunks)
    if save_dir is not None:
        save_wav(save_dir / f"reply-{rid}.wav", chunks, engine.sample_rate)
    emit(
        out,
        {
            "event": "done",
            "id": rid,
            "audio_s": round(samples / engine.sample_rate, 3),
            "total_ms": round((time.perf_counter() - started) * 1000),
        },
    )


def serve(
    engine: Engine,
    player: Player,
    stdin: IO[str],
    out: IO[str],
    save_dir: Path | None = None,
) -> None:
    """Answer requests until `quit` or EOF. A bad line is an error event, never a crash."""
    for line in stdin:
        if not line.strip():
            continue
        try:
            request = json.loads(line)
        except ValueError as e:
            emit(out, {"event": "error", "id": None, "message": f"bad request: {e}"})
            continue
        if not isinstance(request, dict):
            emit(out, {"event": "error", "id": None, "message": "bad request: expected an object"})
            continue
        op = request.get("op")
        if op == "quit":
            return
        if op == "say":
            say(engine, player, request, out, save_dir)
        else:
            emit(out, {"event": "error", "id": request.get("id"), "message": f"unknown op {op!r}"})


def run(
    argv: list[str],
    stdin: IO[str],
    out: IO[str],
    load: Callable[..., Engine] = make_engine,
    speakers: Callable[[int], Player] = Speakers,
) -> int:
    parser = argparse.ArgumentParser(prog="ambient-voice")
    parser.add_argument("--engine", required=True, choices=ENGINES)
    parser.add_argument("--voice", help="the engine's speaker name (default: engine's own)")
    parser.add_argument("--style", help="a standing delivery instruction (Qwen3-TTS only)")
    parser.add_argument(
        "--reference", type=Path, help="a designed voice folder to clone (Qwen3-TTS only)"
    )
    parser.add_argument(
        "--description", help="design a voice from this description, once (Qwen3-TTS only)"
    )
    parser.add_argument("--cue", help="a delivery cue appended to --description when designing")
    parser.add_argument("--no-play", action="store_true", help="generate but do not play")
    parser.add_argument("--save-dir", type=Path, help="also write each reply as reply-<id>.wav")
    args = parser.parse_args(argv)

    started = time.perf_counter()
    try:
        engine = load(
            args.engine, args.voice, args.style, args.reference, args.description, args.cue
        )
    except Exception as e:  # noqa: BLE001 — the parent needs the reason, not a traceback
        print(f"ambient-voice: could not load {args.engine}: {e}", file=sys.stderr)
        return 2
    player: Player = Silent() if args.no_play else speakers(engine.sample_rate)
    if args.save_dir is not None:
        args.save_dir.mkdir(parents=True, exist_ok=True)
    emit(
        out,
        {
            "event": "ready",
            "engine": engine.name,
            "sample_rate": engine.sample_rate,
            "load_ms": round((time.perf_counter() - started) * 1000),
        },
    )
    try:
        serve(engine, player, stdin, out, args.save_dir)
    finally:
        player.close()
    return 0


def main() -> None:
    # The protocol owns stdout. Libraries that print there (progress bars,
    # download notices) would corrupt it, so they get stderr instead.
    protocol = sys.stdout
    sys.stdout = sys.stderr
    sys.exit(run(sys.argv[1:], sys.stdin, protocol))


if __name__ == "__main__":
    main()
