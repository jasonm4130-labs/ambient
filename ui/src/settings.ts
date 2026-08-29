import { Schema } from "effect";

/// What Rust pushes in. Decoded rather than trusted: the page is driven
/// entirely by this payload, so a field that silently arrives as the wrong
/// shape would render a control that lies about the setting behind it.
export const SettingsSchema = Schema.Struct({
  apps: Schema.Array(Schema.String),
  input_device: Schema.NullOr(Schema.String),
  diarize: Schema.Boolean,
  threshold: Schema.Number,
  sessions_dir: Schema.NullOr(Schema.String),
  devices: Schema.Array(Schema.String),
  default_dir: Schema.String,
  ask_before_recording: Schema.Boolean,
  /// Days as a string, or "forever" — the same spelling the config uses, so
  /// keeping audio indefinitely is a word rather than a magic number.
  audio_retention: Schema.String,
  roster: Schema.Array(Schema.String),
  /// Null when the session could not be read — which is not the same as
  /// everyone already having a name, and must not be reported as if it were.
  unnamed: Schema.NullOr(
    Schema.Array(Schema.Struct({ label: Schema.String, sample: Schema.String })),
  ),
  latest_session: Schema.NullOr(Schema.String),
});

export type Settings = Schema.Schema.Type<typeof SettingsSchema>;

export const decodeSettings = Schema.decodeUnknown(SettingsSchema);

/// What the page sends back — only ever the field that changed, because Rust
/// merges each patch onto the config it reads fresh from disk.
export const PatchSchema = Schema.Struct({
  scope: Schema.optional(Schema.Literal("all", "some")),
  input_device: Schema.optional(Schema.String),
  diarize: Schema.optional(Schema.Boolean),
  threshold: Schema.optional(Schema.Number),
  remove_app: Schema.optional(Schema.String),
  action: Schema.optional(Schema.Literal("add_app", "choose_dir")),
  ask_before_recording: Schema.optional(Schema.Boolean),
  audio_retention: Schema.optional(Schema.String),
  add_person: Schema.optional(Schema.String),
  remove_person: Schema.optional(Schema.String),
  assign: Schema.optional(
    Schema.Struct({ label: Schema.String, name: Schema.String, session: Schema.String }),
  ),
});

export type Patch = Schema.Schema.Type<typeof PatchSchema>;

export const empty: Settings = {
  apps: [],
  input_device: null,
  diarize: true,
  threshold: 0.5,
  sessions_dir: null,
  devices: [],
  default_dir: "",
  ask_before_recording: true,
  audio_retention: "7",
  roster: [],
  unnamed: [],
  latest_session: null,
};

/// The threshold merges clusters, so a HIGHER value yields FEWER speakers.
/// The slider reads left-to-right as fewer-to-more, hence the inversion.
const SPAN = 0.8 + 0.3;
export const toSlider = (t: number): number => Math.round((SPAN - t) * 100);
export const fromSlider = (v: number): number => Number((SPAN - v / 100).toFixed(2));
