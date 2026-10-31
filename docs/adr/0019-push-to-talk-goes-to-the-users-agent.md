---
title: "19. Push-to-talk goes to the user's own agent session, through a mailbox `ambient mcp` serves"
sidebar:
  order: 59
---

# 19. Push-to-talk goes to the user's own agent session, through a mailbox `ambient mcp` serves

**Status:** accepted · **Date:** 2026-10-31 · **Supersedes:** —

## Context and Problem Statement

The captain asked for "a push to talk button in the ambient UI, and that puts
in here, then you talk back to me": hold a key in Ambient, speak, have the
words arrive in the Claude Code session he is working in, and hear the answer.
The first build of the app side (PR #112) sent each turn to firstmate's inbox
through `fm-inbox.sh`. firstmate has since been retired, so that destination is
gone, and "in here" was never firstmate anyway: it is the session he types in.

[0018](0018-live-assistant-runs-in-the-users-agent.md) already put the live
assistant's judgement in the user's own Claude Code session, with Ambient
serving tools over `ambient mcp`. Claude Code's channels (a research preview)
let an MCP server push a message into a running session, which arrives as
`<channel source="…">` in the conversation.

## Considered Options

- **A mailbox `ambient mcp` serves, pushed as a channel message.** The
  question waits in a folder; the `ambient mcp` process in the listening
  session pushes it in as a channel notification, and the model answers with a
  `talk_reply` tool. A client without channels loops on `wait_for_talk`.
- **A `talk.command` target such as `claude -p --resume <id>`.** The answer
  comes from a headless fork rather than the session itself, and every turn
  starts a second model run.
- **Retarget to Thrum,** the agent system replacing firstmate. It is still
  being designed, and the mailbox serves whatever session Thrum ends up
  running.
- **Keep firstmate beside a new target.** It is retired; a dead adapter is
  upkeep.

## Decision Outcome

Push-to-talk goes to whichever agent session is listening through `ambient
mcp`. The `ambient talk` worker posts the transcript to `talk/<id>.json`
beside the config file and waits for `talk/<id>.reply.json`. `talk_listen`
makes one `ambient mcp` process the listener, the last one to call it winning;
it keeps the `talk.json` heartbeat fresh and pushes each open question once as
`notifications/claude/channel`. The model answers with `talk_reply`, the first
answer wins, and the worker speaks its first paragraph. The deciding fact was
that the answer then comes from the session the user is already talking to,
with its context and tools, and no second model run or destination setting.

## Consequences

- The question's text is on disk, in an owner-only folder, from the moment it
  is asked until it is answered or the worker gives up (five minutes by
  default). The audio is still never written.
- Only one session hears at a time. A session that was not started with
  `--dangerously-load-development-channels server:ambient` drops the pushed
  message without telling the server; the `talk` prompt's `wait` mode is the
  fallback, and the app cannot tell the two apart.
- Channels are a research preview and may change. The mailbox, `wait_for_talk`
  and `talk_reply` do not depend on them.
- `ambient mcp`'s stdout is no longer only answers: the listener thread
  writes notifications too, one whole line at a time under the same lock.
- `talk.firstmate_home` is gone. An old config still loads; the key is
  dropped at the next save.

## Confirmation

`talk::mailbox::tests` cover the open rule, first-reply-wins, expiry and the
heartbeat; `talk::tests` run the worker against the real mailbox with a thread
standing in for the agent; `mcp::tests` drive a live server over a pipe to
check that a note is pushed once, that only the last listener pushes, and that
notifications never tear an answer line. Delivery into a real Claude Code
session, and the model calling `talk_reply`, have no automated check: they need
a person at a session started with the channels flag.
