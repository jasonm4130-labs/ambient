"""Designed voices: the cache key and the reference folder format, no model."""

import json

from ambient_voice.designed import REFERENCE_TEXT, design_key, load_reference


def test_the_cache_key_changes_with_anything_that_shapes_the_voice():
    base = design_key("An older Scottish man.", "Deadpan.", REFERENCE_TEXT)
    assert base == design_key("An older Scottish man.", "Deadpan.", REFERENCE_TEXT)
    assert base != design_key("An older Welsh man.", "Deadpan.", REFERENCE_TEXT)
    assert base != design_key("An older Scottish man.", None, REFERENCE_TEXT)
    assert base != design_key("An older Scottish man.", "Deadpan.", "Hello.")


def test_a_reference_folder_names_its_clip_and_transcript(tmp_path):
    (tmp_path / "voice.json").write_text(json.dumps({"text": "Hello there.", "audio": "ref.wav"}))
    ref = load_reference(tmp_path)
    assert ref.audio == tmp_path / "ref.wav"
    assert ref.text == "Hello there."
