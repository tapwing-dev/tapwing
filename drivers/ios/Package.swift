// swift-tools-version: 5.9
// Host-side package for running `TapwingRunnerCore` unit tests via `swift test`
// without requiring a booted iOS Simulator. The Xcode runner builds the same
// `Core/` sources directly via `TapwingRunner.xcodeproj` with zero SPM deps.

import PackageDescription

let package = Package(
    name: "TapwingRunnerCore",
    platforms: [
        .macOS(.v13),
        .iOS(.v15),
    ],
    products: [
        .library(name: "TapwingRunnerCore", targets: ["TapwingRunnerCore"]),
    ],
    targets: [
        .target(
            name: "TapwingRunnerCore",
            path: "Core"
        ),
        .testTarget(
            name: "TapwingRunnerCoreTests",
            dependencies: ["TapwingRunnerCore"],
            path: "Tests"
        ),
    ]
)
