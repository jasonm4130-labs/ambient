# When it does not work

Almost every first-run failure here is the same one wearing different clothes:
the recording completes, reports success, and contains silence. macOS does not
warn you, because from its point of view nothing went wrong.

If your call track is flat, work through this page before suspecting the
recogniser or your microphone.

## It recorded silence

The two services then diverge, which is what makes this so confusing:

| Service | csreq matches | Behaviour |
|---|---|---|
| `kTCCServiceMicrophone` | no | keeps working |
| `kTCCServiceAudioCapture` | no | **runs, returns zeros, no error, no prompt** |

These are two independent failures and it is easy to conflate them:

| Launch | Signing | Result |
|---|---|---|
| shell (`build/…/MacOS/ambient`) | anything | **silent** — TCC blames the terminal |
| `open -a` | stale cdhash | **hangs forever** on a prompt nobody sees |
| `open -a` | stable identity | works |

The first row holds regardless of how the app is signed, so fixing the
certificate does not make a terminal launch work — measured after the fix:
direct launch still gave `call peak 0.000`, `open -a` gave `0.869`.

`./setup-signing.sh` fixes this permanently by signing with a certificate, so
the requirement becomes `identifier "uk.ambient.cli" and certificate leaf = …`
and survives rebuilds. `ambient tap` and `ambient record` now detect the silent
case at runtime — they sample `kAudioProcessPropertyIsRunningOutput` during
capture and, if the call track is flat while something was demonstrably playing,
name the likely cause — checking the launch method first (parent pid 1 means
LaunchServices, anything else means a shell), and only then the cdhash.

On a managed Mac a PPPC profile denying `kTCCServiceAudioCapture` produces
**identical** symptoms, so rule the cdhash out before blaming IT.

See [ADR-0001](../adr/0001-core-audio-process-tap.md) and [ADR-0002](../adr/0002-signed-bundle-launch.md).
