#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(unused)]

#[cfg(target_os = "macos")]
use objc::*;

// Generated once with rust-bindgen 0.71 from `bindings.h` against the macOS
// SDK and committed, rather than generated at build time: the crate is also
// cross-compiled for macOS from Linux, where there is no `xcrun` and a stock
// libclang misreads the SDK's TargetConditionals.h. The bindings are a
// handful of stable CoreMedia/CoreVideo/VideoToolbox types and constants,
// identical for arm64 and x86_64 (bindgen's anonymous enum name normalised).
//
// To regenerate on a Mac:
//   bindgen bindings.h -o bindings_generated.rs --no-layout-tests \
//     --allowlist-type 'CMItemIndex|CMSampleTimingInfo|CMVideoCodecType|VTEncodeInfoFlags' \
//     --allowlist-function CMTimeMake \
//     --allowlist-var 'kCVPixelFormatType_.*|kCVReturn.*|VTEncodeInfoFlags_.*|kCMVideoCodecType_.*|kCMTime.*|kCMSampleAttachmentKey_.*' \
//     -- -isysroot "$(xcrun --sdk macosx --show-sdk-path)" -xobjective-c
// then rename each distinct `_bindgen_ty_<n>` to `_bindgen_anon_enum_<k>` in order of
// first appearance (the numbers differ per run; the shapes do not).
#[cfg(target_os = "macos")]
include!("bindings_generated.rs");
