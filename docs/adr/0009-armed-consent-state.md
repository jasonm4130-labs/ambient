# 9. Consent is an Armed menu bar state with per-call opt-in

**Status:** accepted · **Date:** 2026-08-29 · **Supersedes:** —

## Context and Problem Statement

The microphone captures everything audible, which in an office includes the
conversation at the next desk — people who are not in the meeting and have not
agreed to anything. Recording therefore has to be an explicit act at the
moment it starts, not a background default, and the user has to be able to see
that it is happening without clicking anything.

## Considered Options

- **Start on launch** and rely on the Stop item.
- **A `UNUserNotificationCenter` prompt** when a watched app starts audio.
- **A fourth menu bar state, Armed**, entered when a watched app starts
  producing audio, offering *Record this call* or *Not this one*.

## Decision Outcome

Armed. When an app named in `apps` starts producing audio the menu bar moves
to a fourth state and the menu offers the two choices. The rule is a pure
function, `decide` in `src/menubar.rs`, returning `Watch::Arm`, `Watch::Start`,
`Watch::Disarm`, `Watch::Forget` or `Watch::Nothing`, so the consent semantics
are testable without a menu.

Declining is remembered against that bundle and forgotten once it goes quiet,
so saying no to one call does not opt out of the next. A still-playing
declined app is skipped past rather than stopped at, because one left running
all day would otherwise mask every real call behind it. With
`ask_before_recording` off, `decide` returns `Start` instead of `Arm`.

Note the deliberate asymmetry in `apps`: an empty list means "tap everything"
for capture, and "watch nothing" for arming. `decide` returns `Watch::Nothing`
immediately on an empty list, because arming on any sound at all would flap at
every notification chime; the Start item stays the way in.

There is no notification. The bundle carries no entitlements, and the menu bar
is already the consent surface — a state you can see without clicking. A real
`UNUserNotificationCenter` prompt is additive and should be scoped on its own.

## Consequences

`apps` becomes load-bearing for a second, unrelated purpose, so the settings
page must be able to express "selected apps, none picked yet" as distinct from
"everything" — otherwise the list can never be filled and arming can never
trigger.

The check runs on the existing refresh timer, every eighth tick, roughly every
four seconds; enumerating audio processes twice a second would be waste for
something that changes when a human joins a call. Stopping counts as an answer
about that call, so Stop is not undone by the watcher four seconds later.

## Confirmation

`decide` is covered by twelve unit tests in `src/menubar.rs` that run in CI
under `cargo test --all-targets`, including
`an_empty_watch_list_never_arms` (the asymmetry above),
`a_declined_call_is_not_asked_about_again`,
`the_decline_is_forgotten_once_the_call_ends`,
`a_declined_app_does_not_mask_a_later_call` and
`a_call_ending_while_armed_stands_down`.

`cargo run --bin symbolcheck` in CI asks macOS whether each menu bar icon
actually resolves, because `imageWithSystemSymbolName` returning nothing
leaves the previous icon showing — the app would claim to be recording while
merely armed. It caught `waveform.badge.questionmark` not existing.

The live transition is **not** verified: the Core Audio half has never been
exercised end to end, because no app tried could be made to auto-play from a
script. A person must join a real call in a watched app and watch the menu bar
enter Armed.
