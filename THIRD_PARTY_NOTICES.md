# Third-party notices

Ambient source code is licensed under the MIT License in `LICENSE`. This file
covers the model artifacts, native Rust dependencies, UI runtime packages, and
generated documentation assets that the repository fetches or ships. License
texts are included under `licenses/`.

## Model artifacts

### NVIDIA Parakeet TDT 0.6B v3

- Bundle path: `models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/`
- Source artifact: `sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2` from [sherpa-onnx's ASR model release](https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8.tar.bz2).
- Upstream model: [nvidia/parakeet-tdt-0.6b-v3](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)
- License: [CC BY 4.0](licenses/CC-BY-4.0.txt)
- Attribution: NVIDIA Corporation, “Parakeet TDT 0.6B v3”, [upstream model card](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3), licensed under CC BY 4.0.

The release artifact is a sherpa-onnx converted, int8-quantized ONNX model.
Ambient's fetch script extracts it without changing its model files. The
source release URL is recorded; the conversion revision is not recorded.
The measured files and their digests are in
[licenses/model-sha256.txt](licenses/model-sha256.txt).

### pyannote segmentation 3.0

- Bundle path: `models/pyannote-segmentation-3.0/model.onnx`
- Source artifact: `sherpa-onnx-pyannote-segmentation-3-0.tar.bz2` from [sherpa-onnx's speaker-segmentation release](https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/sherpa-onnx-pyannote-segmentation-3-0.tar.bz2).
- Upstream model: [pyannote/segmentation-3.0](https://huggingface.co/pyannote/segmentation-3.0)
- License: MIT; the exact upstream text is [licenses/Pyannote-Segmentation-MIT.txt](licenses/Pyannote-Segmentation-MIT.txt), copyright © 2023 CNRS.

Ambient extracts the converted `model.onnx` and places it under the expected
directory name. The model page gates downloads behind user acceptance of its
access conditions; its published LICENSE grants the MIT rights to use, copy,
modify, merge, publish, distribute, sublicense, and sell the licensed model.
The measured converted file digest is in
[licenses/model-sha256.txt](licenses/model-sha256.txt). The conversion
revision is not recorded in this repository.

### WeSpeaker ResNet34-LM

- Bundle path: `models/wespeaker_en_voxceleb_resnet34_LM.onnx`
- Source artifact: [sherpa-onnx's speaker-recognition release asset](https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx).
- Upstream model: [Wespeaker/wespeaker-voxceleb-resnet34-LM](https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM)
- License: [CC BY 4.0](licenses/CC-BY-4.0.txt)
- Attribution: WeSpeaker project, “wespeaker-voxceleb-resnet34-LM”, [upstream model card](https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM), licensed under CC BY 4.0.

The upstream model card says the model was trained on VoxCeleb2 Dev. That is
dataset provenance, not a replacement for the model's CC BY 4.0 grant; any
separate dataset rights remain with the dataset's licensors. The downloaded
file is the sherpa-onnx ONNX asset and its measured digest is in
[licenses/model-sha256.txt](licenses/model-sha256.txt).

### Silero VAD v4.0

- Bundle path: `models/silero_vad.onnx`
- Source artifact: [silero_vad.onnx at tag v4.0](https://github.com/snakers4/silero-vad/blob/v4.0/files/silero_vad.onnx)
- License: MIT; the exact upstream text is [licenses/Silero-VAD-MIT.txt](licenses/Silero-VAD-MIT.txt), copyright © 2020-present Silero Team.

Ambient downloads this ONNX artifact without modifying it. Its measured digest
is in [licenses/model-sha256.txt](licenses/model-sha256.txt).

## Test audio in model archives

The Parakeet model archive contains `test_wavs/` files. They are not needed at
runtime and their individual source and redistribution terms are not recorded
here. The release bundle omits them. Measurement fixtures fetched by
`scripts/fetch-fixtures` are cached outside the repository and are not shipped.

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

## Rust dependencies

The native dependency inventory covers 94 external packages in Cargo's resolved
`aarch64-apple-darwin` graph, including build dependencies; it is not an exact
list of code linked into the executable. Package names, versions, SPDX expressions,
crates.io source links, and verbatim cached upstream notices are in
[licenses/Rust-Dependencies.txt](licenses/Rust-Dependencies.txt). The
inventory includes the `ort` and `ort-sys` package notices used to integrate
ONNX Runtime. The cached `ort-sys` distribution identifies the native archive
as ONNX Runtime 1.28.0; its upstream notices are included in
[licenses/ONNXRuntime-LICENSE.txt](licenses/ONNXRuntime-LICENSE.txt) and
[licenses/ONNXRuntime-ThirdPartyNotices.txt](licenses/ONNXRuntime-ThirdPartyNotices.txt).

The exact objc2 repository notice is included in the Rust inventory, including
its Apple SDK/Xcode licensing caveat. The recorded realfft source commit has
no LICENSE, COPYING, or NOTICE path and remains listed at the end of the Rust
inventory with its Cargo MIT expression; retrieve that exact release notice
before claiming complete native coverage.

## Sources

- [CC BY 4.0 legal code](https://creativecommons.org/licenses/by/4.0/legalcode)
- [NVIDIA Parakeet model card](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3)
- [pyannote segmentation 3.0 model](https://huggingface.co/pyannote/segmentation-3.0)
- [WeSpeaker model card](https://huggingface.co/Wespeaker/wespeaker-voxceleb-resnet34-LM)
- [Silero VAD v4.0](https://github.com/snakers4/silero-vad/tree/v4.0)
