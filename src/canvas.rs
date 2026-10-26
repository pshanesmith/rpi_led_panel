use std::sync::Arc;

use crate::{
    RGBMatrixConfig,
    color::{CIE1931_LOOKUP, ColorLookup},
    config::K_BIT_PLANES,
    error::InvalidVariantError,
    gpio::Gpio,
    hardware_mapping::HardwareMapping,
    row_address_setter::RowAddressSetter,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Color {
    Red,
    Green,
    Blue,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, strum::EnumString, strum::VariantNames,
)]
#[strum(serialize_all = "UPPERCASE")]
#[strum(
    parse_err_fn = InvalidVariantError::new::<Self>,
    parse_err_ty = InvalidVariantError
)]
pub enum LedSequence {
    #[default]
    Rgb,
    Rbg,
    Grb,
    Gbr,
    Brg,
    Bgr,
}

impl LedSequence {
    /// The order in which the colors appear on the panel's R, G and B inputs.
    const fn colors(self) -> [Color; 3] {
        match self {
            LedSequence::Rgb => [Color::Red, Color::Green, Color::Blue],
            LedSequence::Rbg => [Color::Red, Color::Blue, Color::Green],
            LedSequence::Grb => [Color::Green, Color::Red, Color::Blue],
            LedSequence::Gbr => [Color::Green, Color::Blue, Color::Red],
            LedSequence::Brg => [Color::Blue, Color::Red, Color::Green],
            LedSequence::Bgr => [Color::Blue, Color::Green, Color::Red],
        }
    }

    /// Returns the GPIO bits that drive `color`: the input at the position of `color` in the sequence.
    fn get_gpio(self, color: Color, red_bits: u32, green_bits: u32, blue_bits: u32) -> u32 {
        let inputs = [red_bits, green_bits, blue_bits];
        let position = self
            .colors()
            .iter()
            .position(|c| *c == color)
            .expect("Every sequence contains every color.");
        inputs[position]
    }
}

/// Marks a pixel that is not connected to any LED.
const UNUSED_PIXEL: u32 = u32::MAX;

#[derive(Clone, Copy)]
pub(crate) struct PixelDesignator {
    /// Offset into the bit plane buffer, or [`UNUSED_PIXEL`].
    gpio_word: u32,
    r_bit: u32,
    g_bit: u32,
    b_bit: u32,
    mask: u32,
}

impl PixelDesignator {
    pub(crate) fn new(hardware_mapping: &HardwareMapping, sequence: LedSequence) -> Self {
        let h = hardware_mapping;
        let r = h.panels.red_bits();
        let g = h.panels.green_bits();
        let b = h.panels.blue_bits();
        Self {
            gpio_word: UNUSED_PIXEL,
            r_bit: sequence.get_gpio(Color::Red, r, g, b),
            g_bit: sequence.get_gpio(Color::Green, r, g, b),
            b_bit: sequence.get_gpio(Color::Blue, r, g, b),
            mask: !0u32,
        }
    }

    /// Whether the pixel is connected to an LED.
    pub(crate) fn is_used(&self) -> bool {
        self.gpio_word != UNUSED_PIXEL
    }
}

#[derive(Clone)]
pub(crate) struct PixelDesignatorMap {
    width: usize,
    height: usize,
    pixel_designator: PixelDesignator,
    buffer: Vec<PixelDesignator>,
    /// The bits of LEDs that no visible pixel maps to, by their offset in the first bit plane. They stay off when
    /// filling the canvas.
    hidden_bits: Vec<(usize, u32)>,
}

impl PixelDesignatorMap {
    /// Creates the map for the physical matrix of `config.parallel` chains of `config.chain_length` panels.
    pub(crate) fn new(pixel_designator: PixelDesignator, config: &RGBMatrixConfig) -> Self {
        let width = config.cols * config.chain_length;
        let height = config.rows * config.parallel;
        let mut map = Self::blank(pixel_designator, width, height);
        let hm = config.hardware_mapping;
        let double_rows = config.double_rows();
        for y in 0..height {
            for x in 0..width {
                let position = y * width + x;
                let d = &mut map.buffer[position];
                let offset = (y % double_rows) * (width * K_BIT_PLANES) + x;
                d.gpio_word = u32::try_from(offset).expect("Matrix is too large.");

                let panel = y / config.rows;
                let color_bits = hm.panels.color_bits[panel];
                let (r, g, b) = if y - panel * config.rows < double_rows {
                    (color_bits.r1, color_bits.g1, color_bits.b1)
                } else {
                    (color_bits.r2, color_bits.g2, color_bits.b2)
                };

                d.r_bit = config.led_sequence.get_gpio(Color::Red, r, g, b);
                d.g_bit = config.led_sequence.get_gpio(Color::Green, r, g, b);
                d.b_bit = config.led_sequence.get_gpio(Color::Blue, r, g, b);
                d.mask = !(d.r_bit | d.g_bit | d.b_bit);
            }
        }
        map
    }

    /// Creates a map with every pixel unused, to be filled by a pixel mapper.
    pub(crate) fn blank(pixel_designator: PixelDesignator, width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixel_designator,
            buffer: vec![pixel_designator; width * height],
            hidden_bits: Vec::new(),
        }
    }

    /// Finds the LEDs of the physical `matrix` that this map doesn't reach.
    pub(crate) fn find_hidden_bits(&mut self, matrix: &PixelDesignatorMap) {
        let used = |d: &&PixelDesignator| d.is_used();
        let len = matrix
            .buffer
            .iter()
            .filter(used)
            .map(|d| d.gpio_word as usize + 1)
            .max()
            .unwrap_or(0);
        let mut bits = vec![0u32; len];
        for d in matrix.buffer.iter().filter(used) {
            bits[d.gpio_word as usize] |= !d.mask;
        }
        for d in self.buffer.iter().filter(used) {
            bits[d.gpio_word as usize] &= d.mask;
        }
        self.hidden_bits = bits
            .into_iter()
            .enumerate()
            .filter(|(_, bits)| *bits != 0)
            .collect();
    }

    fn position(&self, x: usize, y: usize) -> Option<usize> {
        (x < self.width && y < self.height).then_some(y * self.width + x)
    }

    pub(crate) fn get(&self, x: usize, y: usize) -> Option<&PixelDesignator> {
        self.position(x, y).map(|position| &self.buffer[position])
    }

    pub(crate) fn get_mut(&mut self, x: usize, y: usize) -> Option<&mut PixelDesignator> {
        self.position(x, y)
            .map(|position| &mut self.buffer[position])
    }

    fn get_pixel_designator(&self) -> PixelDesignator {
        self.pixel_designator
    }

    pub(crate) fn width(&self) -> usize {
        self.width
    }

    pub(crate) fn height(&self) -> usize {
        self.height
    }
}

#[derive(Clone)]
pub struct Canvas {
    cols: usize,
    double_rows: usize,
    bitplane_buffer: Vec<u32>,
    shared_mapper: Arc<PixelDesignatorMap>,
    pwm_bits: usize,
    brightness: u8,
    color_lookup: &'static ColorLookup,
    interlaced: bool,
}

impl Canvas {
    pub(crate) fn new(config: &RGBMatrixConfig, shared_mapper: Arc<PixelDesignatorMap>) -> Self {
        let color_lookup = &*CIE1931_LOOKUP;
        let cols = config.cols * config.chain_length;
        let double_rows = config.double_rows();
        Self {
            cols,
            double_rows,
            bitplane_buffer: vec![0u32; double_rows * cols * K_BIT_PLANES],
            shared_mapper,
            pwm_bits: config.pwm_bits.clamp(1, K_BIT_PLANES),
            brightness: config.led_brightness.clamp(1, 100),
            color_lookup,
            interlaced: config.interlaced,
        }
    }

    #[must_use]
    pub fn height(&self) -> usize {
        self.shared_mapper.height
    }

    #[must_use]
    pub fn width(&self) -> usize {
        self.shared_mapper.width
    }

    fn position_at(&self, double_row: usize, column: usize, bit: usize) -> usize {
        double_row * (self.cols * K_BIT_PLANES) + bit * self.cols + column
    }

    fn row_at(&self, double_row: usize, column: usize, bit: usize) -> &[u32] {
        let start = self.position_at(double_row, column, bit);
        &self.bitplane_buffer[start..start + self.cols]
    }

    fn row_at_mut(&mut self, double_row: usize, column: usize, bit: usize) -> &mut [u32] {
        let start = self.position_at(double_row, column, bit);
        &mut self.bitplane_buffer[start..start + self.cols]
    }

    pub fn set_pixel(&mut self, x: usize, y: usize, r: u8, g: u8, b: u8) {
        let Some(designator) = self.shared_mapper.get(x, y) else {
            return;
        };
        let PixelDesignator {
            gpio_word,
            r_bit,
            g_bit,
            b_bit,
            mask: designator_mask,
        } = *designator;

        if !designator.is_used() {
            return;
        }
        let pos_start = gpio_word as usize;

        let [red, green, blue] = self.color_lookup.lookup_rgb(self.brightness, r, g, b);

        let min_bit_plane = K_BIT_PLANES - self.pwm_bits;

        (min_bit_plane..K_BIT_PLANES).for_each(|plane| {
            let pos = pos_start + self.cols * plane;
            let mask = 1 << plane;
            let mut color_bits = 0;
            if (red & mask) != 0 {
                color_bits |= r_bit;
            };
            if (green & mask) != 0 {
                color_bits |= g_bit;
            };
            if (blue & mask) != 0 {
                color_bits |= b_bit;
            };
            self.bitplane_buffer[pos] &= designator_mask;
            self.bitplane_buffer[pos] |= color_bits;
        });
    }

    pub fn fill(&mut self, r: u8, g: u8, b: u8) {
        let designator = self.shared_mapper.get_pixel_designator();
        let PixelDesignator {
            r_bit,
            g_bit,
            b_bit,
            ..
        } = designator;

        let [red, green, blue] = self.color_lookup.lookup_rgb(self.brightness, r, g, b);

        (K_BIT_PLANES - self.pwm_bits..K_BIT_PLANES).for_each(|b| {
            let mask = 1 << b;
            let mut plane_bits = 0;
            if (red & mask) == mask {
                plane_bits |= r_bit;
            };
            if (green & mask) == mask {
                plane_bits |= g_bit;
            };
            if (blue & mask) == mask {
                plane_bits |= b_bit;
            };
            (0..self.double_rows).for_each(|row| {
                self.row_at_mut(row, 0, b).fill(plane_bits);
            });
            if plane_bits != 0 {
                for &(offset, bits) in &self.shared_mapper.hidden_bits {
                    self.bitplane_buffer[offset + self.cols * b] &= !bits;
                }
            }
        });
    }

    pub(crate) fn dump_to_matrix(
        &self,
        gpio: &mut Gpio,
        hardware_mapping: &HardwareMapping,
        row_setter: &mut dyn RowAddressSetter,
        pwm_low_bit: usize,
        color_clk_mask: u32,
    ) {
        // Depending on if we do dithering, we might not always show the lowest bits.
        let start_bit = (K_BIT_PLANES - self.pwm_bits).max(pwm_low_bit);

        // Round up so that an odd number of rows still shows every row exactly once.
        let half_double = self.double_rows.div_ceil(2);
        for row_loop in 0..self.double_rows {
            let d_row = if self.interlaced {
                if row_loop < half_double {
                    2 * row_loop
                } else {
                    2 * (row_loop - half_double) + 1
                }
            } else {
                row_loop
            };

            // Rows can't be switched very quickly without ghosting, so we do the
            // full PWM of one row before switching rows.
            for b in start_bit..K_BIT_PLANES {
                // While the output enable is still on, we can already clock in the next data.
                let row = self.row_at(d_row, 0, b);
                row.iter().for_each(|col| {
                    gpio.write_masked_bits(*col, color_clk_mask); // col + reset clock
                    gpio.set_bits(hardware_mapping.clock); // Rising edge: clock color in.
                });

                gpio.clear_bits(color_clk_mask); // clock back to normal.

                // OE of the previous row-data must be finished before strobe.
                gpio.wait_pulse_finished();

                // Setting address and strobing needs to happen in dark time.
                row_setter.set_row_address(gpio, d_row);

                // Strobe in the previously clocked in row.
                gpio.set_bits(hardware_mapping.strobe);
                gpio.clear_bits(hardware_mapping.strobe);

                // Now switch on for the sleep time necessary for that bit-plane.
                gpio.send_pulse(b);
            }
        }
    }

    /// Set PWM bits used for output, between 1 and 11. Default is 11, but if you only deal with simple
    /// comic-colors, 1 might be sufficient. Lower values require less CPU.
    ///
    /// The setting carries over to the canvases returned by [`crate::RGBMatrix::update_on_vsync`].
    pub fn set_pwm_bits(&mut self, pwm_bits: usize) {
        let pwm_bits = pwm_bits.clamp(1, K_BIT_PLANES);
        // Planes that become visible may still hold data from long ago.
        for plane in K_BIT_PLANES - pwm_bits..K_BIT_PLANES - self.pwm_bits.min(pwm_bits) {
            for row in 0..self.double_rows {
                self.row_at_mut(row, 0, plane).fill(0);
            }
        }
        self.pwm_bits = pwm_bits;
    }

    #[must_use]
    pub fn pwm_bits(&self) -> usize {
        self.pwm_bits
    }

    /// Set the canvas' brightness in percent. Only affects pixels drawn afterwards.
    ///
    /// The setting carries over to the canvases returned by [`crate::RGBMatrix::update_on_vsync`].
    pub fn set_brightness(&mut self, brightness: u8) {
        self.brightness = brightness.clamp(1, 100);
    }

    #[must_use]
    pub fn brightness(&self) -> u8 {
        self.brightness
    }
}

#[cfg(feature = "drawing")]
pub mod embedded_graphics_support {
    use super::Canvas;
    use embedded_graphics::{
        Pixel,
        draw_target::DrawTarget,
        pixelcolor::Rgb888,
        prelude::{OriginDimensions, RgbColor, Size},
    };

    impl OriginDimensions for Canvas {
        fn size(&self) -> Size {
            Size::new(self.width() as u32, self.height() as u32)
        }
    }

    impl DrawTarget for Canvas {
        type Color = Rgb888;

        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(coord, color) in pixels {
                // `DrawTarget` implementation are required to discard any out of bounds pixels without returning
                // an error or causing a panic.
                if (0..self.width() as i32).contains(&coord.x)
                    && (0..self.height() as i32).contains(&coord.y)
                {
                    self.set_pixel(
                        coord.x as usize,
                        coord.y as usize,
                        color.r(),
                        color.g(),
                        color.b(),
                    );
                }
            }
            Ok(())
        }

        fn clear(&mut self, color: Self::Color) -> Result<(), Self::Error> {
            self.fill(color.r(), color.g(), color.b());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Canvas, Color, LedSequence, PixelDesignator, PixelDesignatorMap};
    use crate::{RGBMatrixConfig, config::K_BIT_PLANES};
    use std::sync::Arc;

    #[test]
    fn led_sequence_matches_upstream() {
        // Upstream drives each color on the input at the color's position in the sequence.
        let (r, g, b) = (1, 2, 4);
        let bits = |seq: LedSequence| {
            [Color::Red, Color::Green, Color::Blue].map(|color| seq.get_gpio(color, r, g, b))
        };
        assert_eq!(bits(LedSequence::Rgb), [r, g, b]);
        assert_eq!(bits(LedSequence::Gbr), [b, r, g]);
        assert_eq!(bits(LedSequence::Brg), [g, b, r]);
        assert_eq!(bits(LedSequence::Bgr), [b, g, r]);
    }

    fn canvas(config: &RGBMatrixConfig) -> Canvas {
        let designator = PixelDesignator::new(&config.hardware_mapping, config.led_sequence);
        Canvas::new(
            config,
            Arc::new(PixelDesignatorMap::new(designator, config)),
        )
    }

    #[test]
    fn increasing_pwm_bits_clears_stale_planes() {
        let mut canvas = canvas(&RGBMatrixConfig::default());
        canvas.fill(255, 255, 255);
        canvas.set_pwm_bits(1);
        canvas.set_pwm_bits(K_BIT_PLANES);
        let low_plane = canvas.row_at(0, 0, 0);
        assert!(low_plane.iter().all(|word| *word == 0));
    }

    #[test]
    fn fill_keeps_hidden_leds_off() {
        // The second panel is left out of the canvas.
        let mut config = RGBMatrixConfig {
            rows: 16,
            cols: 32,
            chain_length: 2,
            pixelmapper: vec!["Remap:32,16|0,0n|0,0x".parse().unwrap()],
            ..Default::default()
        };
        let map = crate::RGBMatrix::create_pixel_map(&mut config).unwrap();
        let mut canvas = Canvas::new(&config, Arc::new(map));
        canvas.fill(255, 255, 255);
        // Only the bits of the first chain are sent to the panels.
        let chain_bits = config.hardware_mapping.panels.color_bits[0].used_bits();
        // The panels are counted from the Raspberry Pi, so the left out one is on the left of the matrix.
        for plane in 0..K_BIT_PLANES {
            let row = canvas.row_at(0, 0, plane);
            assert!(row[..32].iter().all(|word| word & chain_bits == 0));
            assert!(row[32..].iter().all(|word| word & chain_bits == chain_bits));
        }
    }

    #[test]
    fn pwm_bits_are_clamped() {
        let mut canvas = canvas(&RGBMatrixConfig::default());
        canvas.set_pwm_bits(20);
        assert_eq!(canvas.pwm_bits(), K_BIT_PLANES);
        canvas.set_pwm_bits(0);
        assert_eq!(canvas.pwm_bits(), 1);
    }
}
