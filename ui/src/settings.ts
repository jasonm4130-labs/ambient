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
};

/// The threshold merges clusters, so a HIGHER value yields FEWER speakers.
/// The slider reads left-to-right as fewer-to-more, hence the inversion.
const SPAN = 0.8 + 0.3;
export const toSlider = (t: number): number => Math.round((SPAN - t) * 100);
export const fromSlider = (v: number): number => Number((SPAN - v / 100).toFixed(2));
