# Phase 2 Graphics Bootstrap

Phase 2 proves the path from a native window to a visible GPU-presented clear frame. It does not implement Minecraft rendering, shaders, textures, UI, input controls, networking, or gameplay.

## Crate boundaries

- `cubic-app` initializes tracing and starts the platform layer.
- `cubic-platform` owns winit's `ApplicationHandler`, window creation, lifecycle events, and redraw scheduling.
- `cubic-render` owns wgpu's instance, surface, adapter, device, queue, surface configuration, clear pass, and recovery behavior.
- `cubic-core` remains platform-independent and is not coupled to winit or wgpu.

winit provides native window creation and portable application events. wgpu consumes winit's portable raw window handles and selects the compiled graphics backend. Windows builds enable Direct3D 12; ARM64 iOS builds enable Metal.

## Frame and lifecycle behavior

The window is created during winit's `resumed` callback, which is compatible with the iOS application lifecycle. Renderer initialization then selects a surface-compatible high-performance adapter and logs its name and backend. The surface uses a vsynchronized presentation mode.

Redraw events submit a render pass whose only operation is clearing the surface to a dark blue color. The next redraw is requested after the frame, while the event loop otherwise waits. Non-zero resize events reconfigure the surface. Zero-sized and occluded windows skip presentation. Outdated surfaces are reconfigured, lost surfaces are recreated, and GPU out-of-memory errors terminate rendering rather than being treated as recoverable.

## iOS/iPadOS path

The intended path is:

```text
Cubic shared Rust code -> wgpu -> Metal -> iOS/iPadOS display
```

The `cubic-platform::ios` module provides an isolated Rust handoff point for a future native host. CI on macOS installs the `aarch64-apple-ios` standard library, confirms the iPhoneOS SDK exists, compile-checks all workspace targets, and builds the platform library for ARM64 iOS. This catches Rust API, conditional-compilation, dependency, and target-library regressions.

Phase 2 does not include the native Xcode application wrapper required to enter winit through `UIApplicationMain`, an application bundle, Info.plist configuration, signing, provisioning, device installation, or an IPA. Consequently, CI does not prove that the application launches or presents frames on an iPad. Those require a later native wrapper plus simulator/device smoke testing and Apple signing for physical hardware.

Ordinary Windows development does not require a Mac. A Mac with Xcode is required only for local iOS linking, simulator/device work, packaging, and signing; GitHub-hosted macOS CI provides the current compile-time validation.
