---
title: "Talking to your agent"
sidebar:
  order: 18
---

# Talking to your agent

Hold ⌥Space anywhere, say something, and let go. Ambient transcribes what you
said on this Mac and hands the words to the Claude Code session you have
listening, as if you had typed them there. The agent does what you asked and
answers, and Ambient reads the answer aloud. Both sides of every turn are
shown as text in the window. Talking mode is off until you turn it on.

## Setting it up

1. Register `ambient mcp` with Claude Code once, as for
   [the live assistant](mcp.md#registering-it).
2. Start the session you want to talk to with Ambient loaded as a channel:

   ```sh
   claude --dangerously-load-development-channels server:ambient
   ```

   Channels are a Claude Code research preview, and a server of your own needs
   this development flag. Claude Code shows a warning first; choose **I am
   using this for local development**. The flag only works in an interactive
   session, not with `claude -p`.
3. In that session, run `/mcp__ambient__talk`. The agent starts listening and
   says so in one line. The last session to run it is the one that hears; the
   others stop.
4. In Ambient, open **Settings → Talk** and turn on **Talking mode**. Ambient
   now holds ⌥Space. The section says which session is listening, and the
   launch line above is there to copy. Leave **Speak replies** on to hear
   answers, or turn it off to read them only; the status menu has the same
   switch.

The same settings from the command line are `ambient config talk on` and
`ambient config talk.speak off` ([settings](settings.md)). The app picks a
change up within two seconds.

The first time the agent answers, Claude Code may ask permission for the
`mcp__ambient__talk_reply` tool. Allow it, or add it to your allowed tools, so
answers are not held at a prompt you cannot see while you are talking.

### Without channels

In a session started without the flag, or in another MCP client, run the
prompt as `/mcp__ambient__talk wait`. The agent then waits for questions in a
loop with `wait_for_talk`, answering each one, until you tell it to stop. The
session is busy while it waits, so this suits a session kept for talking.

## Talking

- **⌥Space**: hold it while you speak, and let go to send. It works from any
  app. Holding it again while a reply is being read out stops the reply.
- **The window**: the sidebar's **Hold to talk** button works the same way,
  with the mouse or with Space or Return while it has focus. **Conversation**
  opens every turn so far, your words on the right and the reply on the left.
- **The status menu**: **Hold to talk ⌥Space** starts listening when clicked
  and becomes **Send**; click it again to send (a menu cannot be held). The
  line under it shows the last reply, or where the current turn has got to.

The card under **Hold to talk** says where talking is: **Listening…** while
you hold the key, **Thinking…** while the agent works, **Speaking the reply**
while it is read out, or what went wrong. The menu bar icon is a microphone
while Ambient listens and a speaker while a reply is read out.

In the session, your words arrive as a message from Ambient. The agent works
on it as on anything you type, tools and all, then answers. A message that
arrives while the agent is busy waits for its current turn to end.

Only the first paragraph of the answer is read aloud, up to 600 characters;
the rest is shown in the window. The agent is told to lead with one to three
short spoken sentences and put detail after a blank line. Markdown is shown as
plain text, without its symbols.

## When talking is refused

A press beeps and the menu line and the card say why:

- **Talking mode is off.** Turn it on in Settings.
- **Talking is paused while Ambient is recording**, or **while a call is
  waiting to be recorded**. The microphone belongs to the meeting then: what
  you said would land in its transcript, and a spoken reply would be heard by
  everyone on the call. The button reads **Paused: recording** or **Paused:
  call waiting**. A recording that starts while you hold the key drops what
  was heard without sending it, stops a reply being read out, and keeps a
  reply that arrives meanwhile as text.
- **No agent is listening.** Run `/mcp__ambient__talk` in a session. The card's
  **How** link opens Settings.

Less than about 0.4 s of speech is "Didn't catch that", and nothing is sent. A
hold is kept to one minute; at a minute Ambient lets go for you. A question
waits five minutes for its answer; after that the turn says **No reply within
5 minutes; the question expired**, and an answer that comes later is refused,
so the agent knows to say it in the chat instead. A voice that fails, or a
live assistant that is still speaking after ten seconds, leaves the reply as
text with the reason under it.

## What leaves the Mac

The audio is held in memory only, for as long as you hold the key, and is
handed straight to a short-lived `ambient talk` process that transcribes it
with Parakeet on this Mac ([`ambient talk`](commands.md#talk)). It is never
written to disk and never sent anywhere.

The text is written to `talk/` in Ambient's settings folder (`~/Library/Application Support/Ambient/`),
which only your user can read, until the agent answers or the question
expires; then it is deleted ([what is kept](what-is-kept.md#push-to-talk)).
The listening session reads it from there and puts it in its conversation,
which sends it to that agent's model under the client's terms, exactly as if
you had typed it. Ambient keeps no talk log of its own; the window's
conversation lasts until the app quits.

## ⌥Space and permissions

The key is a system hot key (`RegisterEventHotKey`): macOS matches ⌥Space
itself and tells Ambient only when that chord goes down and comes up. It needs
no Accessibility or Input Monitoring permission, because Ambient never sees
any other key. On macOS 26.6.2 it registers with no prompt. Holding the chord
means ⌥Space types nothing in any other app while talking mode is on, which is
why the mode is off by default.

If another app already holds ⌥Space, or macOS refuses the chord, the menu and
the talk card say so and the window's **Hold to talk** button still works.
Turn talking mode off and on again to retry. macOS 15.0 and 15.1 refused
Option-only shortcuts for a while; 15.2 allowed them again.

The microphone is the one Ambient records with (**Settings → Capture**), and
uses the microphone permission Ambient already holds.
