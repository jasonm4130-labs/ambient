/// `config.get`'s payload, and what `config.set` answers back with — the
/// same shape, since the page is a view over the file and never trusts a
/// delta over the whole thing.
export interface SettingsConfig {
  apps: string[];
  input_device: string | null;
  diarize: boolean;
  threshold: number;
  sessions_dir: string | null;
  devices: string[];
  default_dir: string;
  ask_before_recording: boolean;
  /// Days as a string, or "forever" — the same spelling the config uses, so
  /// keeping audio indefinitely is a deliberate word rather than a magic
  /// number.
  audio_retention: string;
  roster: string[];
  latest_session: string | null;
}

export interface UnnamedSpeaker {
  label: string;
  sample: string;
}

/// The threshold merges clusters, so a HIGHER value yields FEWER speakers; the
/// slider reads left-to-right as fewer-to-more, hence the inversion.
const SPAN = 1.1;
export const toSlider = (t: number): number => Math.round((SPAN - t) * 100);
export const fromSlider = (v: number): number => Number((SPAN - v / 100).toFixed(2));
