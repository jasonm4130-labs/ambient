---
title: "Talking to firstmate"
sidebar:
  order: 18
---

# Talking to firstmate

Hold ⌥Space anywhere, say something, and let go. Ambient transcribes what you
said on this Mac, sends the words to firstmate as a note, and reads its reply
aloud when it comes. Both sides of every turn are shown as text in the window.
Talking mode is off until you turn it on.

## Setting it up

In the window, open **Settings → Talk to firstmate**:

1. Choose **firstmate home**: the firstmate checkout whose `bin/fm-inbox.sh`
   takes the note. Without one, talking has nowhere to go and is refused.
2. Turn on **Talking mode**. Ambient now holds ⌥Space.
3. Leave **Speak firstmate's replies** on to hear replies, or turn it off to
   read them only. The status menu has the same switch.

The same settings from the command line are `ambient config talk on`,
`ambient config talk.speak off` and `ambient config talk.firstmate_home <dir>`
([settings](settings.md)). The app picks a change up within two seconds.

## Talking

- **⌥Space**: hold it while you speak, and let go to send. It works from any
  app. Holding it again while a reply is being read out stops the reply.
- **The window**: the sidebar's **Hold to talk** button works the same way,
  with the mouse or with Space or Return while it has focus. **Conversation**
  opens every turn so far, your words on the right and firstmate's on the
  left.
- **The status menu**: **Hold to talk to firstmate ⌥Space** starts listening
  when clicked and sends on the next click (a menu cannot be held). The line
  under it shows the last reply, or where the current turn has got to.

While Ambient listens, the menu bar icon is a microphone; while a reply is
read out, it is a speaker. Less than about 0.4 s of speech is "Didn't catch
that", and nothing is sent. A hold is kept to one minute; at a minute Ambient
lets go for you.

If firstmate is busy the note is queued, and the turn says so. A turn waits
five minutes for its reply. A reply that comes later stays in firstmate's
inbox and is not spoken.

## When talking is paused

Talking is refused while Ambient is recording or while a call is waiting to
be recorded (**Record this call?**). The microphone belongs to the meeting
then: what you said would land in its transcript, and a spoken reply would be
heard by everyone on the call. Pressing ⌥Space then beeps and the menu says
why. A recording that starts, or a call that starts waiting to be recorded,
while you are holding the key drops what was heard without sending it. It
also stops any reply being read out, and a reply that arrives while talking
is paused is shown as text in the window and the menu instead of spoken.

## What leaves the Mac

The audio is held in memory only, for as long as you hold the key, and is
handed straight to a short-lived `ambient talk` process that transcribes it
with Parakeet on this Mac ([`ambient talk`](commands.md#talk)). It is never
written to disk and never sent anywhere. Only the text reaches firstmate's
inbox, which keeps it as a note, exactly as if you had typed it. Ambient keeps
no talk log of its own; the window's conversation lasts until the app quits.

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
