//! Contract tests for the committed six-target sidecar inventory.

use yt_media_engine::{
    manifest::{Distribution, SidecarManifest},
    target::SupportedTarget,
    tool::Tool,
};

const COMMITTED_MANIFEST: &[u8] = include_bytes!("../../../sidecars/manifest.v1.json");

#[test]
fn committed_manifest_is_complete_and_matches_the_baseline() {
    let manifest = SidecarManifest::from_json(COMMITTED_MANIFEST);
    assert!(manifest.is_ok(), "the committed manifest must be valid");
    let Ok(manifest) = manifest else {
        return;
    };

    assert_eq!(manifest.targets.len(), SupportedTarget::ALL.len());
    for target in SupportedTarget::ALL {
        let target_manifest = manifest.target(target);
        assert!(target_manifest.is_some(), "missing target {target}");
        let Some(target_manifest) = target_manifest else {
            continue;
        };

        assert_eq!(target_manifest.tools.len(), Tool::ALL.len());
        for tool in Tool::ALL {
            let tool_manifest = target_manifest.tool(tool);
            assert!(tool_manifest.is_some(), "missing {tool} for {target}");
            let Some(tool_manifest) = tool_manifest else {
                continue;
            };
            assert_eq!(tool_manifest.version, tool.baseline_version());
            assert!(!tool_manifest.executables.is_empty());

            match tool_manifest.distribution {
                Distribution::UpstreamRelease => {
                    assert!(
                        tool_manifest
                            .executables
                            .iter()
                            .all(|executable| executable.sha256.is_some()
                                && executable.size.is_some())
                    );
                }
                Distribution::NativeBuild { .. } => {
                    assert!(matches!(tool, Tool::Ffmpeg | Tool::Ffprobe));
                    assert!(
                        tool_manifest
                            .executables
                            .iter()
                            .all(|executable| executable.sha256.is_none()
                                && executable.size.is_none())
                    );
                }
            }
        }

        let ffmpeg = target_manifest.tool(Tool::Ffmpeg);
        assert!(ffmpeg.is_some());
        let patch = ffmpeg.and_then(|tool| {
            tool.provenance
                .metadata
                .get("x264_source_patch")
                .map(String::as_str)
        });
        if target == SupportedTarget::WindowsArm64 {
            assert_eq!(patch, Some("windows-arm64-sse-arch-guard-v1"));
        } else {
            assert_eq!(patch, None);
        }
    }
}
