use std::fs::read_to_string;

use crate::error::InvalidVariantError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, strum::EnumString, strum::VariantNames)]
#[strum(
    parse_err_fn = InvalidVariantError::new::<Self>,
    parse_err_ty = InvalidVariantError
)]
pub enum PiChip {
    /// Model 0 and 1
    #[strum(serialize = "BCM2708", serialize = "BCM2835")]
    BCM2708,
    /// Models 2 and 3
    #[strum(serialize = "BCM2709", serialize = "BCM2836", serialize = "BCM2837")]
    BCM2709,
    /// Model 4
    #[strum(serialize = "BCM2711")]
    BCM2711,
}

/// Reads the hexadecimal revision code from the "Revision" line of /proc/cpuinfo.
fn revision_from_cpuinfo(cpuinfo: &str) -> Option<u32> {
    let line = cpuinfo.lines().find(|line| line.starts_with("Revision"))?;
    let (_, revision) = line.split_once(':')?;
    u32::from_str_radix(revision.trim(), 16).ok()
}

impl PiChip {
    /// Try to automatically determine the model.
    #[must_use]
    pub fn determine() -> Option<Self> {
        let revision = read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|cpuinfo| revision_from_cpuinfo(&cpuinfo))
            // Kernels that don't list the revision in /proc/cpuinfo still have it in the device tree.
            .or_else(|| {
                let bytes = std::fs::read("/proc/device-tree/system/linux,revision").ok()?;
                Some(u32::from_be_bytes(bytes.get(..4)?.try_into().ok()?))
            })?;
        Self::from_revision(revision)
    }

    /// Get the chip from the revision code, see
    /// <https://www.raspberrypi.com/documentation/computers/raspberry-pi.html#raspberry-pi-revision-codes>.
    fn from_revision(revision: u32) -> Option<Self> {
        // Bits: NOQuuuWuFMMMCCCCPPPPTTTTTTTTRRRR
        //               ^ new style flag
        const NEW_STYLE_FLAG: u32 = 1 << 23;
        if revision & NEW_STYLE_FLAG == 0 {
            // Old style revision codes are only used by the original models.
            return Some(Self::BCM2708);
        }
        // Bits: NOQuuuWuFMMMCCCCPPPPTTTTTTTTRRRR
        //                       ^^^^ processor model
        match (revision >> 12) & 0b1111 {
            // BCM2835
            0 => Some(Self::BCM2708),
            // BCM2836
            1 => Some(Self::BCM2709),
            // BCM2837
            2 => Some(Self::BCM2709),
            // BCM2711
            3 => Some(Self::BCM2711),
            _ => None,
        }
    }

    pub(crate) const fn num_cores(self) -> usize {
        match self {
            PiChip::BCM2708 => 1,
            PiChip::BCM2709 | PiChip::BCM2711 => 4,
        }
    }

    // All peripherals can be described by an offset from the Peripheral Base Address.
    pub(crate) const fn get_peripherals_base(self) -> u64 {
        match self {
            PiChip::BCM2708 => 0x2000_0000,
            PiChip::BCM2709 => 0x3F00_0000,
            PiChip::BCM2711 => 0xFE00_0000,
        }
    }

    pub(crate) fn gpio_slowdown(self) -> u32 {
        match self {
            PiChip::BCM2708 | PiChip::BCM2709 => 1,
            PiChip::BCM2711 => 3,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PiChip, revision_from_cpuinfo};

    #[test]
    fn determines_chip_from_revision() {
        let chip = |cpuinfo| PiChip::from_revision(revision_from_cpuinfo(cpuinfo).unwrap());
        assert_eq!(chip("Revision\t: 000e"), Some(PiChip::BCM2708));
        // Old style revision of an overvolted Pi.
        assert_eq!(chip("Revision\t: 1000002"), Some(PiChip::BCM2708));
        assert_eq!(chip("Revision\t: 9000c1"), Some(PiChip::BCM2708)); // Zero W
        assert_eq!(chip("Revision\t: a01041"), Some(PiChip::BCM2709)); // 2B
        assert_eq!(chip("Revision\t: a02082"), Some(PiChip::BCM2709)); // 3B
        assert_eq!(chip("Revision\t: 902120"), Some(PiChip::BCM2709)); // Zero 2 W
        assert_eq!(chip("Revision\t: c03111"), Some(PiChip::BCM2711)); // 4B
        assert_eq!(chip("Revision\t: c04170"), None); // 5, not supported
        assert_eq!(revision_from_cpuinfo("CPU revision\t: 3"), None);
    }
}
