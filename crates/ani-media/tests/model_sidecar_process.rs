use std::path::Path;
use std::time::Duration;

use ani_media::model_sidecar::{ModelSidecarConfig, ModelSidecarRuntime};
use ani_media::player::{FrameInterpolator, RawVideoFrame};
use sha2::{Digest, Sha256};

#[tokio::test]
async fn launches_validated_sidecar_and_processes_continuous_rgb_frames() {
    let directory = tempfile::tempdir().expect("temporary model bundle");
    let executable_source = Path::new(env!("CARGO_BIN_EXE_ani-model-sidecar-fixture"));
    let executable_name = if cfg!(target_os = "windows") {
        "ani-model-sidecar.exe"
    } else {
        "ani-model-sidecar"
    };
    let executable = directory.path().join(executable_name);
    tokio::fs::copy(executable_source, &executable)
        .await
        .expect("copy fixture executable");
    let model_directory = directory.path().join("models/rife-v4.6");
    tokio::fs::create_dir_all(&model_directory)
        .await
        .expect("create model directory");
    let weight = model_directory.join("flownet.bin");
    tokio::fs::write(&weight, b"fixture-model-weight")
        .await
        .expect("write model weight");
    let executable_digest = digest_file(&executable).await;
    let weight_digest = digest_file(&weight).await;
    tokio::fs::write(
        directory.path().join("manifest.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schemaVersion": 1,
            "protocolVersion": 1,
            "executable": executable_name,
            "executableSha256": executable_digest,
            "model": {
                "modelId": "rife-v4.6",
                "backend": "ncnn-vulkan",
                "directory": "models/rife-v4.6",
                "inputWidth": 2,
                "inputHeight": 2,
                "requiredVramBytes": 1,
                "estimatedFrameTimeMs": 1
            },
            "files": [{
                "path": "models/rife-v4.6/flownet.bin",
                "sha256": weight_digest
            }]
        }))
        .expect("serialize manifest"),
    )
    .await
    .expect("write manifest");

    let mut config = ModelSidecarConfig::new(directory.path().to_path_buf(), 2, 500.0);
    config.frame_timeout = Duration::from_millis(500);
    let runtime = ModelSidecarRuntime::launch(config)
        .await
        .expect("launch validated sidecar");
    assert!(runtime.ready());

    let previous = frame(10, 1_000);
    let next = frame(30, 3_000);
    let result = runtime
        .interpolate(previous, next)
        .await
        .expect("interpolate frame");
    assert_eq!(result.data, vec![20; 12]);
    assert_eq!(result.pts_micros, 2_000);
    let diagnostics = runtime.diagnostics().await;
    assert_eq!(diagnostics.backend, "ncnn-vulkan");
    assert_eq!(diagnostics.gpu_device, "fixture-vulkan-device");
    assert_eq!(diagnostics.processed_frames, 1);
    assert_eq!(diagnostics.dropped_frames, 0);
    runtime.shutdown().await;
}

fn frame(value: u8, pts_micros: i64) -> RawVideoFrame {
    RawVideoFrame {
        width: 2,
        height: 2,
        stride: 6,
        pts_micros,
        data: vec![value; 12],
    }
}

async fn digest_file(path: &Path) -> String {
    let bytes = tokio::fs::read(path).await.expect("read digest input");
    format!("{:x}", Sha256::digest(bytes))
}
