---
title: "The live assistant"
sidebar:
  order: 17
---

# The live assistant

`ambient assist` lets an AI assistant take part in a meeting by voice. It
follows the transcript of the recording in progress, decides when it has
something worth adding, and speaks a short reply through a local voice. It is
**off until you turn it on**, and when it joins a meeting it first says out
loud that an AI is listening and may speak.

It is early software. It sends recent transcript text to cloud models; read
[what leaves this Mac](#what-leaves-this-mac) before using it with anyone who
has not agreed to that.

## What it does

1. Waits for a recording to start, then reads its transcript as it grows,
   through the same API that [`ambient mcp`](mcp.md) serves.
2. Announces itself: "Just so you know, an AI assistant called Claude is
   listening to this meeting and may speak up." While it is in a meeting, the
   status menu shows **AI assistant listening — it may speak**.
3. After each new stretch of transcript, asks a cheap, fast model (default
   Claude Haiku 5.5) whether to speak. The answer is a yes or no and a
   confidence. It says yes when someone addresses the assistant by name, when
   a factual question goes unanswered, or when something clearly wrong is
   about to be acted on.
4. On a confident yes, a stronger model (default Claude Sonnet 5.5) writes one
   to three spoken sentences, and the voice says them.

A **threshold** (how confident the yes must be), a **cooldown** after each
reply and a **cap** per meeting keep it from talking too much. Its own voice,
picked up again by the microphone, is recognised and never treated as a cue.

## Turning it on and running it

Turn it on from the status menu (**Live Assistant**, which shows a tick when it
is on) or with:

```sh
ambient config assistant on
```

Then run it in a terminal, under 1Password so the OpenRouter key never touches
a file:

```sh
op run --env-file .env.assistant.op -- ambient assist
```

`.env.assistant.op` holds only an `op://` reference; its comments give the
`op item create` command for the key. Leave the process running: it joins each
recording when it starts and leaves when it stops. Turning **Live Assistant**
off in the menu silences it immediately, even mid-meeting, and Ctrl-C ends it.

| Flag | Effect |
| --- | --- |
| `--session <id>` | Follow only this session, not whichever one is live. |
| `--silent` | Print replies instead of speaking them. No voice helper starts. |
| `--no-play` | Generate the voice but do not play it. |
| `--save-audio <dir>` | Also write each spoken reply to `<dir>/reply-<n>.wav`. |

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
[design note](../developing/live-assistant.md#measurements). The reply model
picks a mood for each reply (neutral, warm, amused, excited, apologetic or
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
| `assistant.jump_in_model` | `anthropic/claude-haiku-5.5` | Decides whether to speak. Runs often, so keep it cheap and fast. |
| `assistant.reply_model` | `anthropic/claude-sonnet-5.5` | Writes what is said. |
| `assistant.threshold` | `0.75` | Confidence (0–1) the jump-in model needs. |
| `assistant.cooldown_s` | `60` | Seconds of silence after each reply. |
| `assistant.max_per_meeting` | `10` | Most replies in one meeting. |
| `assistant.base_url` | `https://openrouter.ai/api/v1` | OpenAI-compatible endpoint; a Cloudflare AI Gateway URL works. |
| `assistant.zdr` | `true` | Use only zero-data-retention providers. |
| `voice.engine` | `auto` | `auto`, `qwen3-tts`, `supertonic3` or `kokoro`. |
| `voice.speaker` | engine default | `Vivian` (Qwen3-TTS), `F1` (Supertonic), `af_heart` (Kokoro). |
| `voice.style` | none | Standing delivery instruction for preset Qwen3-TTS speakers. |
| `voice.reference` | none | A designed voice folder to clone, such as `voices/v1-gravel`. Relative paths are inside the helper folder. Qwen3-TTS only. |
| `voice.description` | none | Design a voice from this description, once. Used when `voice.reference` is unset. |
| `voice.cue` | none | A delivery cue appended to the description when designing. |
| `voice.helper_dir` | `voice/` in the source tree | Where the helper project is. |

## What leaves this Mac

Audio never does. When the assistant is on and in a meeting, the most recent 40
transcript lines (who spoke, when, and what they said) go to the jump-in model
each time new lines arrive, at most every four seconds; on a yes, the same lines
go to the reply model. Both calls go through OpenRouter, or through the gateway
in `assistant.base_url`.

With `assistant.zdr` on, OpenRouter routes only to providers with a
zero-data-retention policy and refuses providers that train on prompts. If no
such provider serves the model, the call fails and the assistant stays quiet
rather than falling back.

Each decision and reply is appended to `assistant.jsonl` in the session folder,
so what the assistant heard, decided and said is kept with the meeting and
deleted with it.

## Cost

The jump-in model runs on most new stretches of transcript; the reply model
runs only when the assistant speaks. With the defaults (Haiku 5.5 at $0.10 and
$0.50 per million input and output tokens, on OpenRouter's catalogue on
2026-10-08), each jump-in question costs a fraction of a cent. `assistant.jsonl`
records each call's cost as OpenRouter reports it.
