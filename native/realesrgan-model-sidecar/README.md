# Ani Real-ESRGAN model sidecar

This target wraps the pinned `Real-ESRGAN-ncnn-vulkan` implementation with the
Ani binary RGB24 frame protocol. It initializes NCNN/Vulkan and the 2x anime
video model once, then calls `RealESRGAN::process` directly for every in-memory
frame. It never serializes frames through PNG files or temporary directories.

Production bundles are assembled and verified by
`scripts/prepare-realesrgan-model-sidecar.mjs`. A successful source build is not
GPU acceptance: the packaged sidecar must still pass its real Vulkan handshake
and warmup on every release target.
