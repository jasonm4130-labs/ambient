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

/// The roster, as removable chips. Adding is a text field rather than a native
/// panel: a person's name is not a file, so there is nothing to browse for.
const renderPeople = (settings: Settings): void => {
  const people = el("people");
  people.replaceChildren();
  if (settings.roster.length === 0) {
    const none = document.createElement("span");
    none.className = "hint";
    none.textContent = "Nobody yet.";
    people.append(none);
  }
  for (const who of settings.roster) {
    const chip = document.createElement("span");
    chip.className = "chip";
    const name = document.createElement("b");
    name.textContent = who;
    const remove = document.createElement("a");
    remove.className = "x";
    remove.textContent = "×";
    remove.title = `Remove ${who}`;
    remove.addEventListener("click", () => send({ remove_person: who }));
    chip.append(name, remove);
    people.append(chip);
  }
};

/// One row per speaker the last recording could not name, each with the first
/// thing that voice said. Choosing a name applies it to every line of that
/// speaker; there is no guessing, because nothing is stored that could guess.
const renderNaming = (settings: Settings): void => {
  const naming = el("naming");
  const hint = el("namehint");
  naming.replaceChildren();

  if (settings.latest_session === null) {
    hint.textContent = "Nothing recorded yet.";
    return;
  }
  if (settings.unnamed === null) {
    hint.textContent = `${settings.latest_session} could not be read.`;
    return;
  }
  if (settings.unnamed.length === 0) {
    hint.textContent = `Everyone in ${settings.latest_session} has a name.`;
    return;
  }
  hint.textContent = settings.latest_session;

  const session = settings.latest_session;
  for (const speaker of settings.unnamed) {
    const row = document.createElement("div");
    row.className = "row";
    const text = document.createElement("div");
    text.className = "text";
    const label = document.createElement("span");
    label.className = "name";
    label.textContent = speaker.label;
    const said = document.createElement("span");
    said.className = "say";
    said.textContent = speaker.sample === "" ? "(no speech)" : `“${speaker.sample}”`;
    text.append(label, said);

    const pick = document.createElement("select");
    pick.className = "right";
    pick.append(new Option("Not named", ""));
    for (const who of settings.roster) pick.append(new Option(who, who));
    pick.addEventListener("change", () => {
      // The session is sent with the name: a recording can finish while this
      // window is open, and the name must land on the one shown here.
      if (pick.value !== "")
        send({ assign: { label: speaker.label, name: pick.value, session } });
    });

    row.append(text, pick);
    naming.append(row);
  }
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

  el("ask").classList.toggle("on", settings.ask_before_recording);
  el("askhint").textContent = filtered
    ? "Wait to be told when one of these apps starts audio."
    : "Only watches the apps listed above — add one to be asked.";
  // A value set from the CLI need not be one of the offered periods; showing
  // the select blank would misreport the setting behind it.
  const keep = el<HTMLSelectElement>("keep");
  if (![...keep.options].some((o) => o.value === settings.audio_retention)) {
    keep.append(new Option(`${settings.audio_retention} days`, settings.audio_retention));
  }
  keep.value = settings.audio_retention;

  renderPeople(settings);
  renderNaming(settings);
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
  el("ask").addEventListener("click", () =>
    send({ ask_before_recording: !current.ask_before_recording }),
  );
  el<HTMLSelectElement>("keep").addEventListener("change", (e) =>
    send({ audio_retention: (e.target as HTMLSelectElement).value }),
  );

  const person = el<HTMLInputElement>("personname");
  const addPerson = (): void => {
    const who = person.value.trim();
    if (who === "") return;
    person.value = "";
    send({ add_person: who });
  };
  el("addperson").addEventListener("click", addPerson);
  person.addEventListener("keydown", (e) => {
    if ((e as KeyboardEvent).key === "Enter") addPerson();
  });
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
