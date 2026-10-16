"""Where generated audio goes: the speakers, a WAV file, or nowhere.

Playback runs on its own thread fed by a queue, so the engine generates the
next chunk while the current one plays. Writing to the device from the
generating thread would serialise the two and leave a gap between chunks.
"""

from __future__ import annotations

import queue
import threading
import wave
from pathlib import Path
from typing import Protocol

import numpy as np


class Player(Protocol):
    def play(self, chunk: np.ndarray) -> None: ...

    def wait(self) -> None: ...

    def close(self) -> None: ...


class Speakers:
    """Streams chunks to the default output device."""

    def __init__(self, sample_rate: int):
        import sounddevice as sd

        self.stream = sd.OutputStream(samplerate=sample_rate, channels=1, dtype="float32")
        self.stream.start()
        self.chunks: queue.Queue[np.ndarray | None] = queue.Queue()
        self.thread = threading.Thread(target=self._drain, daemon=True)
        self.thread.start()

    def _drain(self) -> None:
        while (chunk := self.chunks.get()) is not None:
            self.stream.write(chunk.reshape(-1, 1))
            self.chunks.task_done()
        self.chunks.task_done()

    def play(self, chunk: np.ndarray) -> None:
        self.chunks.put(chunk)

    def wait(self) -> None:
        """Block until every queued chunk has been handed to the device."""
        self.chunks.join()

    def close(self) -> None:
        self.chunks.put(None)
        self.thread.join(timeout=5)
        self.stream.stop()
        self.stream.close()


class Silent:
    """Discards audio. Used by `--no-play` and by the tests."""

    def play(self, chunk: np.ndarray) -> None:
        pass

    def wait(self) -> None:
        pass

    def close(self) -> None:
        pass


def save_wav(path: Path, chunks: list[np.ndarray], sample_rate: int) -> None:
    """Write float chunks as one 16-bit mono WAV, for listening back later."""
    audio = np.concatenate(chunks) if chunks else np.zeros(0, dtype=np.float32)
    pcm = (np.clip(audio, -1.0, 1.0) * 32767).astype("<i2")
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(sample_rate)
        w.writeframes(pcm.tobytes())
