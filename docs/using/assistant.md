---
title: "The live assistant"
sidebar:
  order: 17
---

# The live assistant

The live assistant lets an AI take part in a meeting by voice. An agent in your
own Claude Code session follows the transcript of the recording in progress,
decides when it has something worth adding, and speaks a short reply through a
local voice on this Mac. The model is whichever one you run in Claude Code, on
your own subscription; Ambient needs no API key for it.

It is **off until you turn it on**, and when it starts watching a meeting it
first says out loud that an AI is listening and may speak.

It is early software. The transcript goes to the model you run in Claude
Code; read [what leaves this Mac](#what-leaves-this-mac) before using it with
anyone who has not agreed to that.

## Setting it up

1. Connect Ambient to Claude Code once, as in [MCP setup](mcp.md):

   ```sh
   claude mcp add ambient -- "$(which ambient)" mcp
   ```

2. Turn the assistant on, from the status menu (**Live Assistant**, which
   shows a tick when it is on) or with:

   ```sh
   ambient config assistant on
   ```

## Using it in a meeting

Start recording, then in Claude Code pick a fast model and run the `watch`
prompt:

```text
claude --model haiku
> /mcp__ambient__watch
```

Anything you type after the command is passed on, for example
`/mcp__ambient__watch Listen out for budget numbers and correct any that are wrong.`

The agent then:

1. Starts watching. Ambient says out loud: "Just so you know, an AI assistant
   called Claude is listening to this meeting and may speak up." While it
   watches, the status menu shows **AI assistant listening — it may speak**.
2. Waits for each burst of speech, reads it, and decides. It speaks when
   someone addresses it by name, when a factual question put to the room goes
   unanswered, or when something clearly wrong is about to be acted on.
   Otherwise it stays quiet.
3. Speaks one to three short sentences through the local voice.
4. Stops when the recording stops, when the assistant is turned off, or when
   you tell it to.

Ambient, not the agent, enforces a **cooldown** after each utterance and a
**cap** per meeting, so a model that loses the thread cannot talk over a
meeting. Its own voice, picked up again by the microphone, is left out of what
the agent reads. Turning **Live Assistant** off in the menu silences it
immediately, even mid-meeting.

Haiku is fast enough to answer about three seconds after its name is said. A
larger model answers more slowly but may judge better when to speak.

## The voice

Speech runs in a separate helper process, so a voice model never shares memory
with the recorder. The helper lives in `voice/` in the source tree and runs
through [uv](https://docs.astral.sh/uv/); its first start installs the chosen
engine's runtime and downloads its model.

`voice.engine` is set per Mac, and `auto` picks by installed memory:

| Engine | Picked by `auto` | First sound | Helper peak memory | Expression |
| --- | --- | --- | --- | --- |
| `qwen3-tts` (Qwen3-TTS 0.6B, MLX) | 32 GB or more | ~0.05–0.1 s (streamed) | ~2.7 GB preset, ~1.6 GB cloned voice | A delivery instruction per reply (preset), or a designed voice |
| `supertonic3` (Supertonic 3, ONNX) | 16–31 GB | ~0.4 s (first sentence) | ~0.6 GB | `<laugh>` / `<sigh>` on amused or apologetic replies |
| `kokoro` (Kokoro-82M int8, ONNX) | under 16 GB | ~0.7 s (first sentence) | ~0.4 GB | None |

The figures were measured on an M5 Max; see the
[design note](../developing/live-assistant.md#measurements). The agent
picks a tone for each utterance (neutral, warm, amused, excited, apologetic or
concerned). With a preset Qwen3-TTS speaker it becomes tone of voice, and
`voice.style` adds a standing instruction to every reply, for example
`ambient config voice.style "Dry and understated."`.

### A voice of your own

Qwen3-TTS can also speak in a voice designed from a description. The voice is
designed once by the larger 1.7B VoiceDesign model, then a small clone of it
speaks every reply:

```sh
ambient config voice.reference voices/v1-gravel
```

That selects the voice shipped in `voice/voices/v1-gravel`: an older Scottish
man with a deep, gravelly voice. Its `voice.json` records the description and
delivery cue it was designed from. To design another, set a description and,
optionally, a cue, and leave `voice.reference` unset:

```sh
ambient config voice.description "A warm, softly spoken woman in her forties with a slight Irish lilt."
ambient config voice.cue "She delivers this calmly, with a smile."
```

The first start then designs the voice, which downloads the 1.7B model and
briefly uses about 5.6 GB. Later starts reuse the design. A cloned voice sounds
the same on every reply. It does not take a per-reply mood, so the reply's
emotion comes through only in its words.

A cloned voice is lighter than a preset one. The helper keeps a slimmed copy
of the model, built on first use. It peaks at about 1.6 GB, settles at about
1.1 GB, and speaks within about 50 ms.

Licences differ: Qwen3-TTS and Kokoro are Apache-2.0; Supertonic 3 is
OpenRAIL-M, which carries use restrictions. Models download to the Hugging
Face cache, `~/.cache/supertonic3` and `~/Library/Caches/Ambient/voice`, not
into the app. Designed voices and the slimmed clone are cached in
`~/Library/Caches/Ambient/voice` too, about 0.9 GB per voice.

## Settings

All are set with `ambient config <key> <value>`; see [settings](settings.md).

| Key | Default | Effect |
| --- | --- | --- |
| `assistant` | `off` | The on/off switch. |
| `assistant.name` | `Claude` | What people call it, and what it answers to. |
| `assistant.cooldown_s` | `60` | Seconds of silence after each utterance. |
| `assistant.max_per_meeting` | `10` | Most utterances in one meeting. |
| `voice.engine` | `auto` | `auto`, `qwen3-tts`, `supertonic3` or `kokoro`. |
| `voice.speaker` | engine default | `Vivian` (Qwen3-TTS), `F1` (Supertonic), `af_heart` (Kokoro). |
| `voice.style` | none | Standing delivery instruction for preset Qwen3-TTS speakers. |
| `voice.reference` | none | A designed voice folder to clone, such as `voices/v1-gravel`. Relative paths are inside the helper folder. Qwen3-TTS only. |
| `voice.description` | none | Design a voice from this description, once. Used when `voice.reference` is unset. |
| `voice.cue` | none | A delivery cue appended to the description when designing. |
| `voice.helper_dir` | `voice/` in the source tree | Where the helper project is. |

To hear the voice without playing it, for a test, start the server as
`ambient mcp --no-play --save-audio <dir>`; each utterance is written to
`<dir>/reply-<n>.wav`.

## What leaves this Mac

Audio never does. While the agent watches, each new stretch of transcript (who
spoke, when, and what they said) goes to the agent, and from there to the model
it runs. With Claude Code that is Anthropic, under the terms of your Claude
plan. Ambient itself sends nothing anywhere: it makes no model call and holds
no key.

Each utterance is appended to `assistant.jsonl` in the session folder, so what
the assistant said is kept with the meeting and deleted with it.

## Cost

The agent runs on your Claude Code plan, so a meeting uses your plan's usage,
not a separate bill. Ambient groups speech into bursts, so the agent takes one
short turn per burst rather than one per line. A 75-second sample meeting took
13 turns with Haiku. Claude Code reported that as $0.006 in API terms, which is
an equivalent figure, not a charge on a subscription.
