//! ambient — local-first capture, transcription and attribution.

use anyhow::{bail, Result};

const USAGE: &str = "\
ambient — local-first ambient capture

USAGE
  ambient record [--name <s>] [--app <bundle-id>]... [--seconds <n>]
                 [--mic-only]          record, then transcribe after Stop;
                                       --mic-only skips the system audio
  ambient stop [<session-dir>]         stop the recording in progress
  ambient sessions [--json]            list the sessions on disk
  ambient search <query> [--json]      find words across every session
  ambient show <session-dir> [--verbatim] [--json]
                                       print a recorded session
  ambient retry <session-dir>          transcribe a session whose job failed
  ambient rerun <session-dir>          transcribe again from the audio; the
                                       old transcript moves to previous/ and
                                       typed names carry over
  ambient name <session-dir> <label> <name>
                                       name a speaker, e.g. call-1 Priya
  ambient undo <session-dir> [--seq <n>]
                                       take back the last naming
  ambient meta <session-dir> name|notes|pinned|tag|untag <value>
                                       set the name/notes, pin, or add/remove a tag
  ambient delete <session-dir> --yes   remove a session and its audio
  ambient export <session-dir> [--format <f>] [--out <path>]
                                       write markdown, text, json, srt, vtt or assistant
  ambient config [<key> <value>]       show or change settings
  ambient mcp                          serve the session library, read-only,
                                       over MCP on stdio
  ambient roster [add|rm <name>]       the people you record with
  ambient doctor [--json]              say which of nine things is missing
  ambient probe                        check this machine is viable
  ambient tap <out.wav> <secs> [bundle-id...]   record both tracks, no bot
  ambient peak <a.wav>...              per-channel peak level of a wav

Sessions are written to ~/Documents/Ambient (override with AMBIENT_HOME).
";

#[derive(Debug, Default, PartialEq)]
struct RecordArgs {
    name: Option<String>,
    seconds: Option<u64>,
    apps: Vec<String>,
    mic_only: bool,
}

fn parse_record_args(mut args: impl Iterator<Item = String>) -> Result<RecordArgs> {
    let mut r = RecordArgs::default();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--name" => r.name = args.next(),
            "--seconds" => {
                r.seconds = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--seconds needs a value"))?
                        .parse()?,
                )
            }
            "--app" => {
                if let Some(b) = args.next() {
                    r.apps.push(b);
                }
            }
            "--mic-only" => r.mic_only = true,
            other => bail!("unexpected argument {other:?}\n\n{USAGE}"),
        }
    }
    if r.mic_only && !r.apps.is_empty() {
        bail!("--mic-only records no system audio, so --app has nothing to select");
    }
    Ok(r)
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        // No arguments: the menu bar app. One signed executable serves both,
        // so `open -a Ambient.app --args …` still reaches the CLI.
        None => ambient::menubar::run(),
        Some("record") => {
            let r = parse_record_args(args)?;
            let dir = ambient::session::record(
                r.name.as_deref(),
                &r.apps,
                r.seconds,
                r.mic_only.then_some(ambient::config::CaptureMode::Mic),
            )?;
            println!("{}", dir.display());
            Ok(())
        }
        Some("stop") => {
            let dir = args.next();
            let stopped =
                ambient::session::stop_recording(dir.as_deref().map(std::path::Path::new))?;
            println!("{}", stopped.display());
            Ok(())
        }
        Some("name") => {
            let dir = args.next().unwrap_or_default();
            let label = args.next().unwrap_or_default();
            let who = args.next().unwrap_or_default();
            if dir.is_empty() || label.is_empty() || who.is_empty() {
                bail!("{USAGE}");
            }
            let path = std::path::Path::new(&dir);
            let _lock = ambient::session::claim_transcription(path)?;
            let n = ambient::session::name_speaker(path, &label, &who)?;
            eprintln!("  {n} line(s) now attributed to {who}");
            ambient::session::show(path, false, false)
        }
        Some("undo") => {
            let dir = args.next().unwrap_or_default();
            if dir.is_empty() {
                bail!("{USAGE}");
            }
            let mut seq: Option<usize> = None;
            while let Some(a) = args.next() {
                match a.as_str() {
                    "--seq" => {
                        seq = Some(
                            args.next()
                                .ok_or_else(|| anyhow::anyhow!("--seq needs a value"))?
                                .parse()?,
                        )
                    }
                    other => bail!("unexpected argument {other:?}\n\n{USAGE}"),
                }
            }
            let path = std::path::Path::new(&dir);
            let _lock = ambient::session::claim_transcription(path)?;
            let n = match seq {
                Some(seq) => {
                    ambient::session::undo_seq(path, seq)?;
                    1
                }
                None => ambient::session::undo_last_naming(path)?,
            };
            eprintln!("reverted {n} edits");
            Ok(())
        }
        Some("meta") => {
            let dir = args.next().unwrap_or_default();
            let field = args.next().unwrap_or_default();
            let value = args.next().unwrap_or_default();
            if dir.is_empty() || field.is_empty() || value.is_empty() {
                bail!("{USAGE}");
            }
            if let Some(other) = args.next() {
                bail!("unexpected argument {other:?}\n\n{USAGE}");
            }
            let dir = std::path::Path::new(&dir);
            let mut patch = ambient::session::MetaPatch::default();
            match field.as_str() {
                "name" => patch.name = Some(value),
                "notes" => patch.notes = Some(value),
                "pinned" => {
                    patch.pinned = Some(match value.as_str() {
                        "true" | "yes" | "on" | "1" => true,
                        "false" | "no" | "off" | "0" => false,
                        other => bail!("{other:?} is not a yes or no. Use true or false."),
                    })
                }
                "tag" => patch.add_tag = Some(value),
                "untag" => patch.remove_tag = Some(value),
                other => bail!("unexpected argument {other:?}\n\n{USAGE}"),
            }
            let _lock = ambient::session::claim_transcription(dir)?;
            let meta = ambient::session::update_meta(dir, &patch)?;
            println!(
                "{}  tags: {}",
                meta.name.as_deref().unwrap_or("-"),
                meta.tags.join(", ")
            );
            Ok(())
        }
        Some("delete") => {
            let dir = args.next().unwrap_or_default();
            if dir.is_empty() {
                bail!("{USAGE}");
            }
            let mut yes = false;
            for a in args.by_ref() {
                match a.as_str() {
                    "--yes" => yes = true,
                    other => bail!("unexpected argument {other:?}\n\n{USAGE}"),
                }
            }
            if !yes {
                bail!("this removes the session and its audio for good; add --yes to confirm");
            }
            let path = std::path::Path::new(&dir);
            let root = path
                .parent()
                .ok_or_else(|| anyhow::anyhow!("{dir:?} has no parent directory"))?;
            let id = path
                .file_name()
                .ok_or_else(|| anyhow::anyhow!("{dir:?} has no session id"))?
                .to_string_lossy()
                .into_owned();
            let lock = ambient::session::claim_transcription(path)?;
            ambient::session::delete(root, &id, &lock)?;
            println!("removed {}", path.display());
            Ok(())
        }
        Some("roster") => {
            let mut names = ambient::roster::load();
            match (args.next().as_deref(), args.next()) {
                (Some("add"), Some(who)) => {
                    if ambient::roster::add(&mut names, &who) {
                        ambient::roster::save(&names)?;
                        eprintln!("  added {who}");
                    } else {
                        eprintln!("  {who} was already on the roster");
                    }
                }
                (Some("rm"), Some(who)) => {
                    if ambient::roster::remove(&mut names, &who) {
                        ambient::roster::save(&names)?;
                        eprintln!("  removed {who}");
                    } else {
                        bail!("{who:?} is not on the roster");
                    }
                }
                (Some(verb @ ("add" | "rm")), None) => {
                    bail!("`ambient roster {verb}` needs a name")
                }
                (Some(other), _) => bail!("unknown roster command {other:?}. Try add or rm"),
                (None, _) => {
                    if names.is_empty() {
                        println!("(nobody yet — ambient roster add <name>)");
                    } else {
                        for n in &names {
                            println!("{n}");
                        }
                    }
                }
            }
            Ok(())
        }
        Some("export") => {
            let dir = args.next().unwrap_or_default();
            if dir.is_empty() {
                bail!("{USAGE}");
            }
            let dir = std::path::Path::new(&dir);
            let mut format_arg: Option<String> = None;
            let mut out: Option<std::path::PathBuf> = None;
            while let Some(a) = args.next() {
                match a.as_str() {
                    "--format" => {
                        format_arg = Some(
                            args.next()
                                .ok_or_else(|| anyhow::anyhow!("--format needs a value"))?,
                        )
                    }
                    "--out" => {
                        out = Some(
                            args.next()
                                .ok_or_else(|| anyhow::anyhow!("--out needs a path"))?
                                .into(),
                        )
                    }
                    other => bail!("unexpected argument {other:?}\n\n{USAGE}"),
                }
            }
            let format: ambient::export::Format = match &format_arg {
                Some(f) => f.parse().map_err(|e| anyhow::anyhow!("{e}\n\n{USAGE}"))?,
                None => ambient::export::Format::Markdown,
            };
            let default_name = match format {
                ambient::export::Format::Markdown => "transcript.md",
                ambient::export::Format::Text => "transcript.txt",
                ambient::export::Format::Json => "transcript.json",
                ambient::export::Format::Srt => "transcript.srt",
                ambient::export::Format::Vtt => "transcript.vtt",
                ambient::export::Format::Assistant => "transcript.assistant.md",
            };
            let out = out.unwrap_or_else(|| dir.join(default_name));
            std::fs::write(&out, ambient::export::render(dir, format)?)?;
            println!("{}", out.display());
            Ok(())
        }
        Some("sessions") => {
            let flags: Vec<String> = args.collect();
            let json = flags.iter().any(|a| a == "--json");
            if let Some(other) = flags.iter().find(|a| *a != "--json") {
                bail!("unexpected argument {other:?}\n\n{USAGE}");
            }
            let home = ambient::session::home();
            let rows = ambient::session::summaries(&home);
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
                return Ok(());
            }
            if rows.is_empty() {
                eprintln!("no sessions in {}", home.display());
                return Ok(());
            }
            for s in &rows {
                println!(
                    "{:<24}  {:<25}  {:>8}  {:<20}  {}",
                    s.id,
                    s.started_at.as_deref().unwrap_or("-"),
                    s.duration_s
                        .map(|d| format!("{d:.1}s"))
                        .unwrap_or_else(|| "-".into()),
                    s.name.as_deref().unwrap_or("-"),
                    s.state()
                );
            }
            Ok(())
        }
        Some("search") => {
            let rest: Vec<String> = args.collect();
            let json = rest.iter().any(|a| a == "--json");
            let mut query_parts: Vec<&str> = Vec::new();
            for a in &rest {
                if a == "--json" {
                    continue;
                }
                if a.starts_with("--") {
                    bail!("unexpected argument {a:?}\n\n{USAGE}");
                }
                query_parts.push(a);
            }
            if query_parts.is_empty() {
                bail!("{USAGE}");
            }
            let query = query_parts.join(" ");
            let home = ambient::session::home();
            let hits = ambient::session::search(&home, &query, 50)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&hits)?);
                return Ok(());
            }
            println!("{:<24}  {:<8}  {:<12}  text", "session", "mm:ss", "speaker");
            for h in &hits {
                let secs = h.start_ms / 1000;
                println!(
                    "{:<24}  {:02}:{:02}     {:<12}  {}",
                    h.session,
                    secs / 60,
                    secs % 60,
                    h.speaker.as_deref().unwrap_or("-"),
                    h.text
                );
            }
            Ok(())
        }
        Some("show") => {
            let dir = args.next().unwrap_or_default();
            if dir.is_empty() {
                bail!("{USAGE}");
            }
            let flags: Vec<String> = args.collect();
            let verbatim = flags.iter().any(|a| a == "--verbatim");
            let json = flags.iter().any(|a| a == "--json");
            ambient::session::show(std::path::Path::new(&dir), verbatim, json)
        }
        Some(verb @ ("retry" | "rerun")) => {
            let dir = args.next().unwrap_or_default();
            if dir.is_empty() || args.next().is_some() {
                bail!("{USAGE}");
            }
            let path = std::path::Path::new(&dir);
            if verb == "retry" {
                ambient::session::transcribe_session(path, None)?;
            } else {
                ambient::session::rerun_session(path, None)?;
            }
            ambient::session::show(path, false, false)
        }
        // stdout is the MCP protocol: anything else printed there breaks it.
        Some("mcp") => {
            if let Some(other) = args.next() {
                bail!("unexpected argument {other:?}\n\n{USAGE}");
            }
            ambient::mcp::serve(
                std::io::stdin().lock(),
                std::io::stdout().lock(),
                &ambient::session::home(),
            )
        }
        Some("doctor") => {
            let flags: Vec<String> = args.collect();
            let json = flags.iter().any(|a| a == "--json");
            if let Some(other) = flags.iter().find(|a| *a != "--json") {
                bail!("unexpected argument {other:?}\n\n{USAGE}");
            }
            let checks = ambient::doctor::run(
                ambient::session::models_root(),
                ambient::engine::locate(),
                &ambient::config::path(),
                &ambient::session::home(),
            );
            let failed = checks.iter().filter(|c| !c.ok).count();
            if json {
                println!("{}", serde_json::to_string_pretty(&checks)?);
            } else {
                for c in &checks {
                    println!(
                        "{:<4} {:<28}  {}",
                        if c.ok { "ok" } else { "FAIL" },
                        c.name,
                        c.detail
                    );
                }
            }
            if failed > 0 {
                bail!("{failed} check(s) failed");
            }
            Ok(())
        }
        Some("probe") => {
            ambient::probe::run()?;
            Ok(())
        }
        Some("tap") => {
            let out = args.next().unwrap_or_default();
            let secs: u64 = args.next().unwrap_or_default().parse().unwrap_or(10);
            let rest: Vec<String> = args.collect();
            let include_mic = !rest.iter().any(|a| a == "--no-mic");
            // Whether a child of the bundle inherits its audio-capture grant.
            // TCC hands back silence, not an error, so only a capture can tell.
            let via_child = rest.iter().any(|a| a == "--via-child");
            let bundles: Vec<String> = rest
                .into_iter()
                .filter(|a| a != "--no-mic" && a != "--via-child")
                .collect();
            if out.is_empty() {
                bail!("{USAGE}");
            }
            if via_child {
                let exe = std::env::current_exe()?;
                eprintln!("spawning child: {} tap {out} {secs}", exe.display());
                let status = std::process::Command::new(&exe)
                    .args(["tap", &out, &secs.to_string()])
                    .status()?;
                eprintln!("child exited: {status}");
                return Ok(());
            }
            let cfg = ambient::config::Config::load();
            let from_config = bundles.is_empty() && !cfg.apps.is_empty();
            let bundles = if from_config {
                cfg.apps.clone()
            } else {
                bundles
            };
            if bundles.is_empty() {
                eprintln!("tapping ALL system audio for {secs}s");
            } else {
                eprintln!(
                    "tapping {} for {secs}s{}",
                    bundles.join(", "),
                    if from_config { " (from settings)" } else { "" }
                );
            }

            let tap = ambient::capture::ProcessTap::start(
                &bundles,
                60,
                include_mic,
                cfg.input_device.as_deref(),
            )?;
            eprintln!(
                "running: room {} Hz x {} ch, call {} Hz x {} ch",
                tap.mic_rate as u32, tap.mic_channels, tap.call_rate as u32, tap.call_channels
            );

            let mut drain = ambient::capture::Drain::default();
            let (mut room, mut call): (Vec<f32>, Vec<f32>) = (Vec::new(), Vec::new());
            let mut max_rendering = 0usize;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            while std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(200));
                let (r, c) = tap.drain(&mut drain);
                room.extend_from_slice(&r);
                call.extend_from_slice(&c);
                max_rendering = max_rendering.max(ambient::probe::processes_rendering_output());
            }

            // Mic and call run on separate clocks, so they get separate files.
            let peak = |v: &[f32]| v.iter().fold(0.0f32, |a, s| a.max(s.abs()));
            let (mic_real, _, call_real, _) = tap.real_seconds(&drain);
            let base = out.strip_suffix(".wav").unwrap_or(&out).to_string();
            for (label, samples, hz, real, path) in [
                (
                    "room",
                    &room,
                    tap.mic_rate,
                    mic_real,
                    format!("{base}.room.wav"),
                ),
                (
                    "call",
                    &call,
                    tap.call_rate,
                    call_real,
                    format!("{base}.call.wav"),
                ),
            ] {
                if samples.is_empty() {
                    eprintln!("  {label}: no audio");
                    continue;
                }
                let spec = hound::WavSpec {
                    channels: 1,
                    sample_rate: hz as u32,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                };
                let mut w = hound::WavWriter::create(&path, spec)?;
                for s in samples.iter() {
                    w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
                }
                w.finalize()?;
                let total = samples.len() as f64 / hz;
                eprintln!(
                    "  {label} peak {:.3} — {path} ({total:.1}s, {real:.1}s real{})",
                    peak(samples),
                    if total - real > 0.2 {
                        format!(", {:.1}s padded", total - real)
                    } else {
                        String::new()
                    }
                );
            }

            if let Some(advice) =
                ambient::capture::silent_tap_advice(peak(&call), max_rendering, &bundles)
            {
                eprintln!("\nWARNING: {advice}\n");
            }
            Ok(())
        }
        // Needs no audio permission: a bundle launch via `open -a` discards
        // stdout, so the wav is the only evidence a capture made sound.
        // setup-signing.sh depends on this.
        Some("peak") => {
            let wavs: Vec<String> = args.collect();
            if wavs.is_empty() {
                bail!("{USAGE}");
            }
            let mut silent = 0usize;
            for wav in &wavs {
                let mut r = hound::WavReader::open(wav)?;
                let spec = r.spec();
                let ch = spec.channels.max(1) as usize;
                let mut peaks = vec![0.0f32; ch];
                for (i, s) in r.samples::<i16>().enumerate() {
                    let v = (s? as f32 / 32768.0).abs();
                    let c = i % ch;
                    if v > peaks[c] {
                        peaks[c] = v;
                    }
                }
                let shown = peaks
                    .iter()
                    .map(|p| format!("{p:.4}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                let quiet = peaks.iter().all(|p| *p < ambient::capture::SILENT_PEAK);
                if quiet {
                    silent += 1;
                }
                println!(
                    "{wav}: {} Hz, {ch} ch, peak {shown}{}",
                    spec.sample_rate,
                    if quiet { "  SILENT" } else { "" }
                );
            }
            if silent == wavs.len() {
                bail!(
                    "every wav was silent (peak < {})",
                    ambient::capture::SILENT_PEAK
                );
            }
            Ok(())
        }
        Some("config") => {
            let key = args.next();
            let mut cfg = ambient::config::Config::load();
            match (key, args.next()) {
                (Some(k), Some(v)) => {
                    ambient::config::refuse_while_live(
                        &k,
                        ambient::session::live_session().as_deref(),
                    )?;
                    cfg.set(&k, &v)?;
                    cfg.save()?;
                    println!("{} = {}", k, v);
                    eprintln!("written to {}", ambient::config::path().display());
                }
                (Some(k), None) => bail!("`ambient config {k}` needs a value"),
                (None, _) => {
                    println!(
                        "{:<14} {}",
                        "apps",
                        if cfg.apps.is_empty() {
                            "(all system audio)".to_string()
                        } else {
                            cfg.apps.join(", ")
                        }
                    );
                    println!(
                        "{:<14} {}",
                        "input_device",
                        cfg.input_device
                            .clone()
                            .unwrap_or_else(|| "(system default)".into())
                    );
                    println!("{:<14} {}", "diarize", cfg.diarize);
                    println!("{:<14} {}", "capture", cfg.capture.as_str());
                    println!(
                        "{:<14} {}",
                        "ask_before_recording", cfg.ask_before_recording
                    );
                    println!(
                        "{:<14} {}",
                        "audio_retention_days",
                        match cfg.audio_retention_days {
                            None => "forever".to_string(),
                            Some(0) => "0 (deleted once transcribed)".to_string(),
                            Some(n) => format!("{n}"),
                        }
                    );
                    println!(
                        "{:<14} {}",
                        "sessions_dir",
                        cfg.sessions_dir
                            .clone()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "(~/Documents/Ambient)".into())
                    );
                    println!();
                    println!(
                        "in effect: sessions go to {}",
                        ambient::session::home().display()
                    );
                    if std::env::var_os("AMBIENT_HOME").is_some() {
                        println!("           (AMBIENT_HOME is set and overrides sessions_dir)");
                    }
                    println!("file:      {}", ambient::config::path().display());
                    println!();
                    println!("input devices:");
                    for (_, n) in ambient::capture::input_devices() {
                        println!("  {n}");
                    }
                }
            }
            Ok(())
        }
        _ => {
            print!("{USAGE}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<RecordArgs> {
        parse_record_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn record_captures_everything_unless_told_otherwise() {
        assert!(!parse(&[]).unwrap().mic_only);
        assert!(!parse(&["--name", "x", "--seconds", "5"]).unwrap().mic_only);
    }

    #[test]
    fn mic_only_is_a_flag_beside_the_others() {
        let r = parse(&["--name", "standup", "--mic-only", "--seconds", "30"]).unwrap();
        assert!(r.mic_only);
        assert_eq!(r.name.as_deref(), Some("standup"));
        assert_eq!(r.seconds, Some(30));
    }

    #[test]
    fn mic_only_with_an_app_is_refused() {
        let e = parse(&["--mic-only", "--app", "us.zoom.xos"]).unwrap_err();
        assert!(e.to_string().contains("--mic-only"), "{e}");
    }

    #[test]
    fn an_unknown_record_flag_is_still_refused() {
        assert!(parse(&["--mic"]).is_err());
    }
}
