"""The helper's wire protocol, driven with a fake engine: no model, no audio device."""

import io
import json

import numpy as np
import pytest

from ambient_voice.__main__ import run, serve
from ambient_voice.engines import QWEN_INSTRUCTIONS, sentences
from ambient_voice.player import Silent


class FakeEngine:
    name = "fake"
    sample_rate = 10

    def __init__(self, fail_on: str | None = None):
        self.fail_on = fail_on
        self.heard: list[tuple[str, str]] = []

    def stream(self, text, emotion):
        self.heard.append((text, emotion))
        if self.fail_on and self.fail_on in text:
            raise RuntimeError("engine fell over")
        for _ in sentences(text):
            yield np.zeros(5, dtype=np.float32)


class Recording(Silent):
    def __init__(self):
        self.played = 0
        self.closed = False

    def play(self, chunk):
        self.played += len(chunk)

    def close(self):
        self.closed = True


def events(out: io.StringIO) -> list[dict]:
    return [json.loads(line) for line in out.getvalue().splitlines()]


def test_say_reports_first_audio_then_done_with_the_audio_length():
    engine, out = FakeEngine(), io.StringIO()
    request = {"op": "say", "id": 7, "text": "One. Two.", "emotion": "warm"}
    serve(engine, Silent(), io.StringIO(json.dumps(request) + "\n"), out)
    got = events(out)
    assert [e["event"] for e in got] == ["speaking", "done"]
    assert got[0]["id"] == 7 and got[1]["id"] == 7
    assert got[1]["audio_s"] == 1.0  # two 5-sample chunks at 10 Hz
    assert engine.heard == [("One. Two.", "warm")]


def test_a_bad_line_is_an_error_event_and_the_loop_reads_on():
    out = io.StringIO()
    lines = "not json\n[1]\n" + json.dumps({"op": "dance", "id": 2}) + "\n"
    lines += json.dumps({"op": "say", "id": 3, "text": "Still here."}) + "\n"
    serve(FakeEngine(), Silent(), io.StringIO(lines), out)
    got = events(out)
    assert [e["event"] for e in got] == ["error", "error", "error", "speaking", "done"]
    assert got[2]["id"] == 2


def test_an_engine_failure_is_reported_against_its_request_and_does_not_end_the_helper():
    out = io.StringIO()
    lines = json.dumps({"op": "say", "id": 1, "text": "boom"}) + "\n"
    lines += json.dumps({"op": "say", "id": 2, "text": "fine"}) + "\n"
    serve(FakeEngine(fail_on="boom"), Silent(), io.StringIO(lines), out)
    got = events(out)
    assert got[0] == {"event": "error", "id": 1, "message": "RuntimeError: engine fell over"}
    assert got[-1]["event"] == "done" and got[-1]["id"] == 2


def test_empty_text_is_refused():
    out = io.StringIO()
    serve(FakeEngine(), Silent(), io.StringIO('{"op":"say","id":1,"text":"  "}\n'), out)
    assert events(out) == [{"event": "error", "id": 1, "message": "nothing to say"}]


def test_quit_stops_reading():
    out = io.StringIO()
    lines = '{"op":"quit"}\n{"op":"say","id":1,"text":"never"}\n'
    serve(FakeEngine(), Silent(), io.StringIO(lines), out)
    assert events(out) == []


def test_run_announces_ready_then_serves_and_closes_the_player(tmp_path):
    out, player = io.StringIO(), Recording()
    stdin = io.StringIO('{"op":"say","id":1,"text":"Hello there."}\n')
    code = run(
        ["--engine", "kokoro", "--save-dir", str(tmp_path)],
        stdin,
        out,
        load=lambda *args: FakeEngine(),
        speakers=lambda rate: player,
    )
    assert code == 0
    got = events(out)
    assert got[0]["event"] == "ready" and got[0]["engine"] == "fake"
    assert got[-1]["event"] == "done"
    assert player.played == 5 and player.closed
    assert (tmp_path / "reply-1.wav").stat().st_size > 44


def test_run_exits_non_zero_without_a_ready_line_when_the_engine_cannot_load():
    out = io.StringIO()

    def broken(*args):
        raise OSError("weights missing")

    assert run(["--engine", "qwen3-tts"], io.StringIO(""), out, load=broken) == 2
    assert out.getvalue() == ""


def test_an_unknown_engine_is_refused_by_the_argument_parser():
    with pytest.raises(SystemExit):
        run(["--engine", "espeak"], io.StringIO(""), io.StringIO())


def test_sentences_split_on_terminal_punctuation():
    assert sentences("Yes. It closed Tuesday! Drop it?  ") == [
        "Yes.",
        "It closed Tuesday!",
        "Drop it?",
    ]
    assert sentences("no punctuation") == ["no punctuation"]


def test_every_reply_emotion_has_a_qwen_instruction():
    # The Rust side's emotion list must stay a subset of these keys.
    assert set(QWEN_INSTRUCTIONS) == {
        "neutral",
        "warm",
        "amused",
        "excited",
        "apologetic",
        "concerned",
    }
