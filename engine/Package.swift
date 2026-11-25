// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "AmbientEngine",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "ambient-engine", targets: ["AmbientEngine"])],
    dependencies: [
        // Pinned to the revision benchmarked in issue #168; v0.18.4 lacks the later
        // decoder fixes.
        .package(url: "https://github.com/FluidInference/FluidAudio.git",
                 revision: "e2ea498dafd6db9b3f18f0f644bc35312b3a7679"),
    ],
    targets: [
        // No model inside, so CI can test it without the weights or a Neural Engine.
        .target(name: "EngineCore"),
        .executableTarget(
            name: "AmbientEngine",
            dependencies: ["EngineCore", .product(name: "FluidAudio", package: "FluidAudio")]
        ),
        .testTarget(name: "EngineCoreTests", dependencies: ["EngineCore"]),
    ]
)
