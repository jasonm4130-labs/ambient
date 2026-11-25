export interface SettingsConfig {
  apps: string[];
  input_device: string | null;
  diarize: boolean;
  sessions_dir: string | null;
  devices: string[];
  default_dir: string;
  ask_before_recording: boolean;
  audio_retention: string;
  roster: string[];
  latest_session: string | null;
}

export interface UnnamedSpeaker {
  label: string;
  sample: string;
}
