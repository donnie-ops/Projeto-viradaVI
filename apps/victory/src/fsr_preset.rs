#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FsrPreset {
    Quality,
    Balanced,
    Performance,
}

impl FsrPreset {
    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name {
            "quality" => Some(Self::Quality),
            "balanced" => Some(Self::Balanced),
            "performance" => Some(Self::Performance),
            _ => None,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Quality => "quality",
            Self::Balanced => "balanced",
            Self::Performance => "performance",
        }
    }

    // AMD SDK FFX_UPSCALE_QUALITY_MODE values and ratios.
    pub(super) fn mode(self) -> u8 {
        match self {
            Self::Quality => 1,
            Self::Balanced => 2,
            Self::Performance => 3,
        }
    }

    pub(super) fn render_scale(self) -> f32 {
        match self {
            Self::Quality => 2.0 / 3.0,
            Self::Balanced => 1.0 / 1.7,
            Self::Performance => 0.5,
        }
    }

    pub(super) fn render_size(self) -> [u32; 2] {
        [3840.0, 2160.0].map(|size| (size * self.render_scale()) as u32)
    }
}
