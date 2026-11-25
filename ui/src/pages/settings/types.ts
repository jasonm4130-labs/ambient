/// `config.get`'s payload, and what `config.set` answers back with — the
/// same shape, since the page is a view over the file and never trusts a
/// delta over the whole thing.
export interface SettingsConfig {
  apps: string[];
  input_device: string | null;
  diarize: boolean;
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
