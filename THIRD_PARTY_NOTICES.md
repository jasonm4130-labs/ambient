# Third-party notices

Ambient source code is licensed under the MIT License in `LICENSE`. This file
covers the model artifacts, native Rust dependencies, UI runtime packages, and
generated documentation assets that the repository fetches or ships. License
texts are included under `licenses/`.

## Model artifacts

The app runs the two models below, through `ambient-engine` (ADR 0020).

### Parakeet Ultra (Core ML)

- Bundle path: `models/parakeet-ultra/`
- Source artifact: [FluidInference/parakeet-ultra-coreml](https://huggingface.co/FluidInference/parakeet-ultra-coreml), fetched by `ambient-engine fetch`.
- Upstream models: [moondream/parakeet-ultra](https://huggingface.co/moondream/parakeet-ultra), a post-training of [nvidia/parakeet-tdt-0.6b-v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)
- License: [CC BY 4.0](licenses/CC-BY-4.0.txt)
- Attribution: Moondream, “Parakeet Ultra”, based on NVIDIA Corporation's “Parakeet TDT 0.6B v3”; Core ML conversion by Fluid Inference. Licensed under CC BY 4.0.

The encoder is int8 per-channel and the decoder and joint are fp16, as Fluid
Inference published them; Ambient does not change the files. Their digests are
in [licenses/model-sha256.txt](licenses/model-sha256.txt).

### Nemotron 3 Diarization (Core ML)

- Bundle path: `models/nemotron-3-diarization/` (the `fast32` preset only)
- Source artifact: [FluidInference/nemotron-3-diarization-coreml](https://huggingface.co/FluidInference/nemotron-3-diarization-coreml), fetched by `ambient-engine fetch`.
- Upstream model: [nvidia/Nemotron-3-Diarization](https://huggingface.co/nvidia/Nemotron-3-Diarization)
- License: [OpenMDW License Agreement 1.1](licenses/OpenMDW-1.1.txt)
- Attribution: NVIDIA Corporation, “Nemotron 3 Diarization”; Core ML conversion by Fluid Inference. Licensed under OpenMDW-1.1.

OpenMDW-1.1 requires a copy of the agreement with any distribution of the model
materials, which is `licenses/OpenMDW-1.1.txt` in the bundle. Digests are in
[licenses/model-sha256.txt](licenses/model-sha256.txt).

## Measurement fixtures

Measurement fixtures fetched by `scripts/fetch-fixtures` are cached outside
the repository and are not shipped.

## Documentation assets

### Mermaid

The generated documentation runtime is `docs-site/public/mermaid.min.js`,
generated from npm package `mermaid@11.17.2` in `.github/scripts/package.json`.

- License: MIT, copyright © 2014–2022 Knut Sveidqvist.
- Exact text: [licenses/Mermaid-MIT.txt](licenses/Mermaid-MIT.txt)
- Source project: [mermaid-js/mermaid](https://github.com/mermaid-js/mermaid)

### Inter

The shipped `docs-site/public/fonts/Inter-Bold.ttf` is from npm package
`@fontsource-variable/inter@5.3.0` and the Inter Project.

- License: SIL Open Font License 1.1, copyright © 2016 The Inter Project Authors.
- Exact text and copyright header: [licenses/OFL-1.1.txt](licenses/OFL-1.1.txt)
- Source project: [rsms/inter](https://github.com/rsms/inter)

### JetBrains Mono

The documentation site imports JetBrains Mono from npm package
`@fontsource-variable/jetbrains-mono@5.3.0`.

- License: SIL Open Font License 1.1, copyright © 2020 The JetBrains Mono Project Authors.
- Exact text and copyright header: [licenses/OFL-1.1-JetBrains-Mono.txt](licenses/OFL-1.1-JetBrains-Mono.txt)
- Source project: [JetBrains/JetBrainsMono](https://github.com/JetBrains/JetBrainsMono)

### React and ReactDOM

The packaged settings UI bundles npm packages `react@19.2.8` and
`react-dom@19.2.8`.

- License: MIT, copyright © Meta Platforms, Inc. and affiliates.
- Exact text: [licenses/React-MIT.txt](licenses/React-MIT.txt)
- Source projects: [facebook/react](https://github.com/facebook/react)

### Lucide icons

The packaged settings UI bundles icons from npm package `lucide-react@1.41.0`.

- Primary license: ISC, copyright © 2026 Lucide Icons and Contributors.
- Derived Feather icons also carry their upstream MIT notice, copyright © 2013-present Cole Bemis.
- Exact package notices: [licenses/Lucide-ISC-and-Feather-MIT.txt](licenses/Lucide-ISC-and-Feather-MIT.txt)
- Source project: [lucide-icons/lucide](https://github.com/lucide-icons/lucide)

### Tailwind CSS

The settings UI and documentation styles compile npm package
`tailwindcss@4.3.3`. The shipped CSS is generated from that package.

- License: MIT, copyright © Tailwind Labs, Inc.
- Exact text: [licenses/Tailwind-MIT.txt](licenses/Tailwind-MIT.txt)
- Source project: [tailwindlabs/tailwindcss](https://github.com/tailwindlabs/tailwindcss)

The app and documentation dependencies retain their upstream license notices;
the files above cover the identified runtime packages and generated assets.

## Model helper (`ambient-engine`)

`Contents/MacOS/ambient-engine` is built from `engine/` and statically links
[FluidAudio](https://github.com/FluidInference/FluidAudio) at revision
`e2ea498dafd6db9b3f18f0f644bc35312b3a7679`, licensed under the
[Apache License 2.0](licenses/FluidAudio-Apache-2.0.txt). FluidAudio's own
third-party notices, including fastcluster and NeMo text processing, are
reproduced verbatim in [licenses/FluidAudio-ThirdParty.txt](licenses/FluidAudio-ThirdParty.txt).

## Rust dependencies

The native dependency inventory covers 56 external packages in Cargo's resolved
`aarch64-apple-darwin` graph, including build dependencies; it is not an exact
list of code linked into the executable. Package names, versions, SPDX expressions,
crates.io source links, and verbatim cached upstream notices are in
[licenses/Rust-Dependencies.txt](licenses/Rust-Dependencies.txt).
`scripts/check-rust-notices` checks it against `Cargo.lock`.

The exact objc2 repository notice is included in the Rust inventory, including
its Apple SDK/Xcode licensing caveat. The recorded realfft source commit has
no LICENSE, COPYING, or NOTICE path and remains listed at the end of the Rust
inventory with its Cargo MIT expression; retrieve that exact release notice
before claiming complete native coverage.

## Sources

- [CC BY 4.0 legal code](https://creativecommons.org/licenses/by/4.0/legalcode)
- [NVIDIA Parakeet model card](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)
