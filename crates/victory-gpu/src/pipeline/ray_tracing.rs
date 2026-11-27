//! Choose the console ray pipeline at process startup. With no master setting,
//! keep the original independent developer controls (including builder quarantine).
use super::Shader;
use std::sync::OnceLock;
use victory_shader::{Container, FourCC};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Work {
    Builder,
    Traversal,
    Activate,
    RayBins,
    Resolve,
    Other,
}

#[derive(Clone, Copy)]
pub(super) struct Policy {
    master: Option<bool>,
    static_bvh: bool,
    pub(super) builder: bool,
    activate: bool,
    resolve: bool,
    pub(super) quarantine: bool,
}

impl Policy {
    fn new(
        master: Option<bool>,
        builder: bool,
        activate: bool,
        resolve: bool,
        quarantine: bool,
    ) -> Self {
        Self {
            master,
            static_bvh: false,
            builder: master.unwrap_or(builder),
            activate: master.unwrap_or(activate),
            resolve: master.unwrap_or(resolve || builder),
            quarantine: master.is_none() && quarantine,
        }
    }

    fn static_bvh() -> Self {
        Self { static_bvh: true, ..Self::new(None, false, true, true, false) }
    }

    pub(super) fn master_off(self) -> bool {
        self.master == Some(false)
    }

    pub(super) fn skips_passes(self) -> bool {
        self.static_bvh || self.master == Some(false) || !self.activate || !self.resolve
    }

    pub(super) fn omission(self, pass: Work) -> Option<&'static str> {
        match pass {
            Work::Builder if self.static_bvh => Some("static BVH mode: dynamic structure builder omitted"),
            Work::Builder if self.master == Some(false) => {
                Some("ray tracing disabled: acceleration structure builder")
            }
            Work::RayBins if self.master_off() => Some("ray tracing disabled: ray bin preparation"),
            Work::Traversal if self.master == Some(false) => {
                Some("ray tracing disabled: BVH traversal")
            }
            Work::Activate if !self.activate => Some("ray activation disabled"),
            Work::Resolve if !self.resolve => Some("ray hit resolve disabled"),
            _ => None,
        }
    }
}

pub(super) fn policy() -> Policy {
    static POLICY: OnceLock<Policy> = OnceLock::new();
    *POLICY.get_or_init(|| {
        let is = |key: &str, value: &str| std::env::var_os(key).is_some_and(|v| v == value);
        let requested = std::env::var("VICTORY_RAY_TRACING");
        let master = match requested.as_deref() {
            Ok("0") => Some(false),
            Ok("1") => Some(true),
            _ => None,
        };
        let policy = if requested.as_deref() == Ok("static-bvh") {
            Policy::static_bvh()
        } else {
            Policy::new(
                master,
                is("VICTORY_BVH_BUILDER", "1"),
                !is("VICTORY_ACTIVATE_RAYS", "0"),
                !is("VICTORY_RT_RESOLVE", "0"),
                is("VICTORY_BVH_QUARANTINE", "1"),
            )
        };
        tracing::info!(
            ?master,
            static_bvh = policy.static_bvh,
            builder = policy.builder,
            activation = policy.activate,
            traversal = master != Some(false),
            resolve = policy.resolve,
            quarantine = policy.quarantine,
            "configured ray-tracing passes"
        );
        policy
    })
}

pub(super) fn work(shader: &Shader) -> Work {
    *shader
        .ray_work
        .get_or_init(|| classify(shader.stage, &shader.container, policy()))
}

fn classify(stage: &str, container: &[u8], mode: Policy) -> Work {
    if stage == "compute" {
        let named = |name: &[u8]| container.windows(name.len()).any(|w| w == name);
        if !mode.resolve && named(b"CS_RaytracingResolve") {
            return Work::Resolve;
        }
        if !mode.activate && named(b"CS_ScreenProbes_ActivateRays") {
            return Work::Activate;
        }
    } else if mode.master != Some(false) {
        return Work::Other;
    }
    let Ok(container) = Container::parse(container) else {
        return Work::Other;
    };
    if mode.master_off()
        && stage == "compute"
        && container.part(FourCC::PSV0).is_some_and(|psv| {
            psv.data.split(|b| *b == 0).any(|entry| {
                [
                    b"CS_ScreenProbes_EvaluateRayRequests_RayBinning_".as_slice(),
                    b"CS_RayBudgeting_Cull_RayBinning_",
                    b"CS_ScreenProbes_GenerateRayBins_",
                    b"CS_GenerateRayBins_",
                    b"CS_GenerateRayBinOffsets_",
                ]
                .iter()
                .any(|name| entry.starts_with(name))
            })
        })
    {
        return Work::RayBins;
    }
    let programs = container
        .part(FourCC::DXIL)
        .and_then(|p| victory_shader::native_programs(p.data).ok());
    let Some(programs) = programs else {
        return Work::Other;
    };
    if stage == "compute" && programs.iter().any(victory_shader::spill::is_spilled) {
        return Work::Builder;
    }
    if mode.master == Some(false) && programs.iter().any(|p| traverses(&p.code)) {
        return Work::Traversal;
    }
    Work::Other
}

fn traverses(code: &[u8]) -> bool {
    // The fast scan excludes programs without either MIMG header. Only the
    // decoder can distinguish a real instruction from a literal or unreachable data.
    code.chunks_exact(4).any(|bytes| {
        let word = u32::from_le_bytes(bytes.try_into().unwrap());
        word >> 26 == 0x3c && (word >> 18) & 0x7e == 0x66 && word & 1 != 0
    }) && victory_shader::gfx10::decode(code).is_ok_and(|instructions| {
        instructions.iter().any(|i| {
            matches!(
                i.name(),
                Some("image_bvh_intersect_ray" | "image_bvh64_intersect_ray")
            )
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_off_overrides_old_stage_switches_and_leaves_other_passes_enabled() {
        let off = Policy::new(Some(false), true, true, true, false);
        for pass in [
            Work::Builder,
            Work::Traversal,
            Work::Activate,
            Work::RayBins,
            Work::Resolve,
        ] {
            assert!(off.omission(pass).is_some());
        }
        assert!(off.omission(Work::Other).is_none());
        assert!(!off.builder);
        assert_eq!(
            classify("compute", b"CS_RaytracingResolve", off),
            Work::Resolve
        );
        assert_eq!(
            classify("compute", b"CS_ScreenProbes_ActivateRays", off),
            Work::Activate
        );
        assert_eq!(
            classify("compute", b"CS_ScreenProbes_Solve", off),
            Work::Other
        );
        assert_eq!(classify("pixel", b"CS_RaytracingResolve", off), Work::Other);
        let on = Policy::new(Some(true), false, false, false, true);
        assert!(on.builder && !on.skips_passes() && !on.quarantine);
        let legacy = Policy::new(None, true, false, false, true);
        assert!(legacy.builder && legacy.resolve && legacy.quarantine);
        assert!(legacy.omission(Work::Activate).is_some());
        assert!(legacy.omission(Work::Traversal).is_none());
        let partial = Policy::static_bvh();
        assert!(!partial.builder && partial.skips_passes());
        assert!(partial.omission(Work::Builder).is_some());
        for work in [Work::Traversal, Work::Activate, Work::RayBins, Work::Resolve] {
            assert!(partial.omission(work).is_none());
        }
    }

    #[test]
    fn only_executed_bvh_opcodes_identify_a_traversal_shader() {
        let run = |words: &[u32]| {
            traverses(
                &words
                    .iter()
                    .flat_map(|w| w.to_le_bytes())
                    .collect::<Vec<_>>(),
            )
        };
        for header in [0xf198_9f07, 0xf19c_9f07] {
            assert!(run(&[
                header, 0x020d01, 0x05040302, 0x09080706, 0x000c0b0a, 0xbf810000
            ]));
            assert!(!run(&[0xbe98_03ff, header, 0xbf810000])); // scalar literal
            assert!(!run(&[0xbf810000, header])); // unreachable tail
        }
    }
}
