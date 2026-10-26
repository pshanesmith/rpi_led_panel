use crate::{RGBMatrixConfig, error::InvalidVariantError, gpio::Gpio};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::EnumString, strum::VariantNames)]
#[strum(
    parse_err_fn = InvalidVariantError::new::<Self>,
    parse_err_ty = InvalidVariantError
)]
pub enum PanelType {
    FM6126,
    FM6127,
}

impl PanelType {
    pub(crate) fn run_init_sequence(self, gpio: &mut Gpio, config: &RGBMatrixConfig) {
        match self {
            Self::FM6126 => Self::init_fm6126(gpio, config),
            Self::FM6127 => Self::init_fm6127(gpio, config),
        }
    }

    fn init_fm6126(gpio: &mut Gpio, config: &RGBMatrixConfig) {
        let hm = &config.hardware_mapping;
        // Address bit 'A' is always on.
        let bits_on = hm.panels.used_bits() | hm.a;
        let bits_off = hm.a;

        let init_b12 = 0b0111_1111_1111_1111; // full bright
        let init_b13 = 0b0000_0000_0100_0000; // panel on

        gpio.clear_bits(hm.clock | hm.strobe);
        Self::write_register(gpio, config, bits_on, bits_off, init_b12, 12);
        Self::write_register(gpio, config, bits_on, bits_off, init_b13, 13);
    }

    /// The FM6217 is very similar to the FM6216. FM6217 adds Register 3 to allow for automatic bad pixel
    /// suppression.
    fn init_fm6127(gpio: &mut Gpio, config: &RGBMatrixConfig) {
        let hm = &config.hardware_mapping;
        let bits_on = hm.panels.color_bits[0].used_bits();
        let bits_off = 0;

        let init_b12 = 0b1111_1111_1100_1110; // register 1
        let init_b13 = 0b1110_0000_0110_0010; // register 2.
        let init_b11 = 0b0101_1111_0000_0000; // register 3.

        gpio.clear_bits(hm.clock | hm.strobe);
        Self::write_register(gpio, config, bits_on, bits_off, init_b12, 12);
        Self::write_register(gpio, config, bits_on, bits_off, init_b13, 13);
        Self::write_register(gpio, config, bits_on, bits_off, init_b11, 11);
    }

    /// Clocks the 16 bit `value` (most significant bit first) into every panel of the chain, latching it into
    /// the register selected by the number of clocks during which the strobe is held high.
    fn write_register(
        gpio: &mut Gpio,
        config: &RGBMatrixConfig,
        bits_on: u32,
        bits_off: u32,
        value: u16,
        strobe_clocks: usize,
    ) {
        let hm = &config.hardware_mapping;
        let columns = config.cols * config.chain_length;
        let mask = bits_on | bits_off | hm.strobe;
        (0..columns).for_each(|c| {
            let mut output = if register_bit(value, c) {
                bits_on
            } else {
                bits_off
            };
            // Same as `c > columns - strobe_clocks`, without the underflow for short chains.
            if c + strobe_clocks > columns {
                output |= hm.strobe;
            };
            gpio.write_masked_bits(output, mask);
            gpio.set_bits(hm.clock);
            gpio.clear_bits(hm.clock);
        });
        gpio.clear_bits(hm.strobe);
    }
}

/// The register bit that is sent at the given column, most significant bit first.
fn register_bit(value: u16, column: usize) -> bool {
    value & (1 << (15 - column % 16)) != 0
}

#[cfg(test)]
mod tests {
    use super::register_bit;

    #[test]
    fn register_bits_are_sent_most_significant_first() {
        // The upstream library describes the FM6126 "panel on" register as the string "0000000001000000".
        let sent: String = (0..32)
            .map(|c| {
                if register_bit(0b0000_0000_0100_0000, c) {
                    '1'
                } else {
                    '0'
                }
            })
            .collect();
        assert_eq!(sent, "00000000010000000000000001000000");
    }
}
