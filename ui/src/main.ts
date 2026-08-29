import { Effect, Layer, ManagedRuntime } from "effect";
import { Bridge, BridgeLive } from "./bridge";
import { type Patch, type Settings, decodeSettings, empty, fromSlider, toSlider } from "./settings";

const runtime = ManagedRuntime.make(Layer.mergeAll(BridgeLive));

/// Last decoded state. The page is a view over what Rust holds, so this is
/// only ever written by `applyConfig` — never by a control reporting what the
/// user just did.
let current: Settings = empty;

const el = <T extends HTMLElement>(id: string): T => {
  const found = document.getElementById(id);
  if (found === null) throw new Error(`no #${id} in the page`);
  return found as T;
};

const send = (patch: Patch) => runtime.runFork(Effect.flatMap(Bridge, (b) => b.send(patch)));

const renderChips = (settings: Settings): void => {
  const chips = el("chips");
  chips.replaceChildren();
  for (const id of settings.apps) {
    const chip = document.createElement("span");
    chip.className = "chip";
    const name = document.createElement("b");
    name.textContent = id;
    const remove = document.createElement("a");
    remove.className = "x";
    remove.textContent = "×";
    remove.title = `Stop capturing ${id}`;
    remove.addEventListener("click", () => send({ remove_app: id }));
    chip.append(name, remove);
    chips.append(chip);
  }
  const add = document.createElement("button");
  add.className = "add";
  add.textContent = "+ Add";
  add.addEventListener("click", () => send({ action: "add_app" }));
  chips.append(add);
};

const renderMics = (settings: Settings): void => {
  const mic = el<HTMLSelectElement>("mic");
  mic.replaceChildren();
  const fallback = new Option("System default", "default");
  mic.append(fallback);
  for (const device of settings.devices) mic.append(new Option(device, device));
  mic.value = settings.input_device ?? "default";
};

const render = (settings: Settings): void => {
  current = settings;
  const filtered = settings.apps.length > 0;

  el<HTMLSelectElement>("scope").value = filtered ? "some" : "all";
  el("chips").style.display = filtered ? "flex" : "none";
  renderChips(settings);
  renderMics(settings);

  el("diarize").classList.toggle("on", settings.diarize);
  el("sensrow").style.opacity = settings.diarize ? "1" : "0.4";
  const sens = el<HTMLInputElement>("sens");
  sens.disabled = !settings.diarize;
  sens.value = String(toSlider(settings.threshold));

  el("dir").textContent = settings.sessions_dir ?? settings.default_dir;
};

const wire = (): void => {
  el<HTMLSelectElement>("scope").addEventListener("change", (e) => {
    const value = (e.target as HTMLSelectElement).value;
    if (value === "all" || value === "some") send({ scope: value });
  });
  el<HTMLSelectElement>("mic").addEventListener("change", (e) =>
    send({ input_device: (e.target as HTMLSelectElement).value }),
  );
  el("diarize").addEventListener("click", () => send({ diarize: !current.diarize }));
  el<HTMLInputElement>("sens").addEventListener("change", (e) =>
    send({ threshold: fromSlider(Number((e.target as HTMLInputElement).value)) }),
  );
  el("choosedir").addEventListener("click", () => send({ action: "choose_dir" }));
};

/// Rust's entry point, called as `applyConfig({...})` through
/// `evaluateJavaScript`. A payload that does not decode leaves the page showing
/// what it showed before rather than a half-applied set of controls.
const applyConfig = (raw: unknown): void => {
  runtime.runFork(
    decodeSettings(raw).pipe(
      Effect.map(render),
      Effect.catchAll((error) =>
        Effect.sync(() => {
          console.error("settings payload rejected", error);
        }),
      ),
    ),
  );
};

declare global {
  interface Window {
    applyConfig: (raw: unknown) => void;
  }
}

window.applyConfig = applyConfig;
wire();
render(empty);
