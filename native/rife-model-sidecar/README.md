# Ani RIFE model sidecar

This target wraps the pinned `rife-ncnn-vulkan` in a long-running binary RGB24
protocol. The RIFE model and Vulkan device are initialized once. Video frames,
not PNG paths, are then processed through `RIFE::process`.

The source checkout and model weights are intentionally prepared outside Git by
`scripts/prepare-rife-model-sidecar.mjs`. Production capability remains disabled
unless the generated bundle manifest, executable, every model file, Vulkan
handshake, and real warmup frame all pass runtime validation.
