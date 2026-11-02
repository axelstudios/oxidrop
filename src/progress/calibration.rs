//! Timings learned from finished files, so later files can estimate stages that log nothing
//! countable (reductions, libdeflate's final compression, a zopfli block's first pass).

/// Rates are seconds per byte of filtered image data.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Calibration {
    pub reducing: Option<f64>,
    /// libdeflate's single final compression.
    pub compressing: Option<f64>,
    pub zopfli: Option<ZopfliTiming>,
}

/// How long zopfli's passes take.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ZopfliTiming {
    /// How many later passes one first pass costs (the first pass also builds the match cache).
    pub first_pass_weight: f64,
    pub first_pass_per_byte: f64,
    pub later_pass_per_byte: f64,
}

impl Calibration {
    /// Blends what one more file measured into what was known; newer files count half, so
    /// the estimates follow changes in the kind of images being optimized.
    #[must_use]
    pub fn merge(self, newer: Self) -> Self {
        fn blend(old: Option<f64>, new: Option<f64>) -> Option<f64> {
            match (old, new) {
                (Some(old), Some(new)) => Some(f64::midpoint(old, new)),
                (old, new) => new.or(old),
            }
        }
        let zopfli = match (self.zopfli, newer.zopfli) {
            (Some(old), Some(new)) => Some(ZopfliTiming {
                first_pass_weight: f64::midpoint(old.first_pass_weight, new.first_pass_weight),
                first_pass_per_byte: f64::midpoint(
                    old.first_pass_per_byte,
                    new.first_pass_per_byte,
                ),
                later_pass_per_byte: f64::midpoint(
                    old.later_pass_per_byte,
                    new.later_pass_per_byte,
                ),
            }),
            (old, new) => new.or(old),
        };
        Self {
            reducing: blend(self.reducing, newer.reducing),
            compressing: blend(self.compressing, newer.compressing),
            zopfli,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_measurements_count_half() {
        let old = Calibration {
            reducing: Some(2.0),
            compressing: None,
            zopfli: None,
        };
        let new = Calibration {
            reducing: Some(4.0),
            compressing: Some(1.0),
            zopfli: None,
        };
        let merged = old.merge(new);
        assert_eq!(merged.reducing, Some(3.0));
        assert_eq!(merged.compressing, Some(1.0));
        assert_eq!(merged.zopfli, None);
        assert_eq!(
            merged.merge(Calibration::default()),
            merged,
            "nothing new keeps what was known"
        );
    }
}
