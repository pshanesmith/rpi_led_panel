//! Raspberry Pi 5 backend that sets the GPIO pins from the CPU through the registered I/O (RIO) block of the RP1.

use std::{fs::OpenOptions, time::Instant};

use memmap2::{MmapMut, MmapOptions};

use crate::{
    RGBMatrixConfig,
    canvas::Canvas,
    config::K_BIT_PLANES,
    hardware_mapping::low_pins,
    init_sequence::PanelInit,
    registers::store_barrier,
    rgb_matrix::MatrixCreationError,
    rp1::{RP1_GPIO_COUNT, Rp1Layout, bitplane_active_words},
};

/// The physical address of the RP1 GPIO, RIO and pad registers.
const PI5_PERIPHERAL_BASE: u64 = 0x1f_000d_0000;
const MAP_SIZE_BYTES: usize = 0x40000;
/// Each GPIO has a status and a control register.
const GPIO_CTRL_OFFSET: usize = 0x4;
const GPIO_STRIDE: usize = 0x8;
const RIO_OUT_OFFSET: usize = 0x10000;
const RIO_OE_OFFSET: usize = 0x10004;
/// Writes to these aliases set or clear the written bits only.
const RIO_SET_ALIAS: usize = 0x2000;
const RIO_CLR_ALIAS: usize = 0x3000;
/// The first pad register selects the voltage, the pins follow.
const PADS_OFFSET: usize = 0x20004;
const GPIO_FUNCTION_SYS_RIO: u32 = 5;
const PAD_FAST_DRIVE: u32 = 0x15;
const PAD_SLOW_DRIVE: u32 = 0x01;
const BASE_PIXEL_CLOCK_HZ: f64 = 20_000_000.0;

pub(crate) struct Rp1Rio {
    registers: RioRegisters,
    layout: Rp1Layout,
    /// For how many pixel clocks each bit plane is shown.
    active_words: [usize; K_BIT_PLANES],
    /// The row address and remaining on-time of the last bit plane of the previous frame, which are shown while
    /// the first bit plane of the next frame is clocked in.
    pending: Option<(u32, usize)>,
}

impl Rp1Rio {
    pub(crate) fn new(config: &RGBMatrixConfig) -> Result<Self, MatrixCreationError> {
        let layout = Rp1Layout::new(config, true)?;
        let slowdown = config.slowdown.unwrap_or(1).max(1) as usize;
        let word_ns = 1e9 / (BASE_PIXEL_CLOCK_HZ / slowdown as f64);
        let map = map_registers().ok_or(MatrixCreationError::MemoryAccessError)?;
        let mut registers = RioRegisters {
            map,
            slowdown,
            word_ns,
            clock: layout.clock,
        };
        registers.configure_pins(&layout);
        Ok(Self {
            registers,
            layout,
            active_words: bitplane_active_words(config, word_ns),
            pending: None,
        })
    }

    pub(crate) fn init_panels(&mut self, init: &PanelInit) {
        let blank = self.layout.output_enable;
        for &word in init.registers.iter().flatten() {
            self.registers.write_clocked_word(low_pins(word) | blank);
        }
        self.registers.write_pins(blank);
    }

    pub(crate) fn dump(&mut self, canvas: &Canvas, pwm_low_bit: usize) {
        dump_frame(
            &self.layout,
            &self.active_words,
            &mut self.pending,
            canvas,
            pwm_low_bit,
            &mut self.registers,
        );
    }
}

impl Drop for Rp1Rio {
    fn drop(&mut self) {
        self.registers.write_pins(self.layout.output_enable);
        self.registers.setup_delay();
        self.registers
            .write(RIO_CLR_ALIAS + RIO_OE_OFFSET, self.layout.used_bits);
        store_barrier();
    }
}

fn map_registers() -> Option<MmapMut> {
    // gpiomem0 gives access to the RP1 GPIO registers without root privileges.
    [
        ("/dev/gpiomem0", 0),
        ("/dev/gpiomem0", PI5_PERIPHERAL_BASE),
        ("/dev/mem", PI5_PERIPHERAL_BASE),
    ]
    .into_iter()
    .find_map(|(path, offset)| {
        let file = OpenOptions::new().read(true).write(true).open(path).ok()?;
        unsafe {
            MmapOptions::new()
                .offset(offset)
                .len(MAP_SIZE_BYTES)
                .map_mut(&file)
                .ok()
        }
    })
}

/// Where a frame is written to: the RP1 registers, or a recording in the tests.
trait RioOutput {
    /// Sets all pins that are driven by RIO.
    fn write_pins(&mut self, pins: u32);
    /// Presents the pins and clocks them in with a rising clock edge.
    fn write_clocked_word(&mut self, pins: u32);
    /// Writes the pin states one after the other, e.g. to shift in a row address.
    fn write_states(&mut self, states: &[u32]);
    /// Keeps the pins as they are for as long as it takes to clock in `words` words.
    fn hold(&mut self, words: usize);
}

struct RioRegisters {
    map: MmapMut,
    /// How many short delays to wait after each change of the pins.
    slowdown: usize,
    /// How long one pixel clock takes.
    word_ns: f64,
    clock: u32,
}

impl RioRegisters {
    #[inline(always)]
    fn write(&mut self, byte_offset: usize, value: u32) {
        unsafe {
            self.map
                .as_mut_ptr()
                .add(byte_offset)
                .cast::<u32>()
                .write_volatile(value);
        }
    }

    #[inline(always)]
    fn setup_delay(&self) {
        for _ in 0..self.slowdown {
            nop();
            nop();
        }
    }

    fn configure_pins(&mut self, layout: &Rp1Layout) {
        for pin in 0..RP1_GPIO_COUNT as usize {
            let bit = 1 << pin;
            if layout.used_bits & bit == 0 {
                continue;
            }
            self.write(GPIO_CTRL_OFFSET + pin * GPIO_STRIDE, GPIO_FUNCTION_SYS_RIO);
            // Clock and latch have the sharpest edges, so they get a gentler drive than the other outputs.
            let drive = if bit == layout.clock || bit == layout.strobe {
                PAD_SLOW_DRIVE
            } else {
                PAD_FAST_DRIVE
            };
            self.write(PADS_OFFSET + pin * 4, drive);
            self.write(RIO_SET_ALIAS + RIO_OE_OFFSET, bit);
        }
        store_barrier();
        self.write_pins(layout.output_enable);
    }
}

impl RioOutput for RioRegisters {
    #[inline(always)]
    fn write_pins(&mut self, pins: u32) {
        self.write(RIO_OUT_OFFSET, pins);
    }

    #[inline(always)]
    fn write_clocked_word(&mut self, pins: u32) {
        self.write_pins(pins);
        self.setup_delay();
        self.write_pins(pins | self.clock);
        self.setup_delay();
    }

    fn write_states(&mut self, states: &[u32]) {
        for &state in states {
            self.write_pins(state);
            self.setup_delay();
        }
    }

    fn hold(&mut self, words: usize) {
        busy_wait_ns((words as f64 * self.word_ns) as u64);
    }
}

fn dump_frame(
    layout: &Rp1Layout,
    active_words: &[usize; K_BIT_PLANES],
    pending: &mut Option<(u32, usize)>,
    canvas: &Canvas,
    pwm_low_bit: usize,
    out: &mut impl RioOutput,
) {
    let start_bit = canvas.start_bit(pwm_low_bit);
    let double_rows = canvas.double_rows();
    let Rp1Layout {
        output_enable,
        strobe,
        ..
    } = *layout;
    let uses_shift_register = layout.uses_shift_register();
    let shifts_every_bitplane = layout.shifts_every_bitplane();

    let (mut previous_addr, mut previous_active_words) = pending.unwrap_or_else(|| {
        let last_row = canvas.display_row(double_rows - 1);
        (layout.row_address_bits(last_row), 0)
    });
    let mut last_shift_register_row = None;

    for row_loop in 0..double_rows {
        let display_row = canvas.display_row(row_loop);
        let current_addr = layout.row_address_bits(display_row);

        for (bit, &bit_active_words) in active_words.iter().enumerate().skip(start_bit) {
            // The previous bit plane stays on while the data of this one is clocked in.
            let mut remaining_overlap_words = previous_active_words;
            for &col in canvas.row_at(display_row, 0, bit) {
                // The RP1 has 28 pins, so the bits of chains above 31 are dropped.
                let mut pins = low_pins(col) | previous_addr;
                if remaining_overlap_words == 0 {
                    pins |= output_enable;
                } else {
                    remaining_overlap_words -= 1;
                }
                out.write_clocked_word(pins);
            }
            if remaining_overlap_words > 0 {
                out.write_pins(previous_addr);
                out.hold(remaining_overlap_words);
            }

            if uses_shift_register
                && (shifts_every_bitplane || last_shift_register_row != Some(display_row))
            {
                out.write_states(&layout.shift_row_address_states(display_row));
                last_shift_register_row = Some(display_row);
            }

            // Latch the data while the LEDs are off.
            out.write_states(&[
                current_addr | output_enable,
                current_addr | output_enable | strobe,
                current_addr | output_enable,
            ]);

            previous_addr = current_addr;
            previous_active_words = bit_active_words;
        }
    }

    // The last bit plane is shown while the next frame is clocked in, instead of waiting for it here.
    *pending = Some((previous_addr, previous_active_words));
    out.write_pins(previous_addr | output_enable);
}

#[inline(always)]
fn nop() {
    #[cfg(any(target_arch = "aarch64", target_arch = "arm"))]
    unsafe {
        std::arch::asm!("nop", options(nomem, nostack, preserves_flags));
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "arm")))]
    std::hint::spin_loop();
}

fn busy_wait_ns(nanos: u64) {
    if nanos == 0 {
        return;
    }
    if nanos <= 250 {
        for _ in 0..8 {
            nop();
        }
        return;
    }
    let start = Instant::now();
    while (start.elapsed().as_nanos() as u64) < nanos {
        std::hint::spin_loop();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Arc;

    use super::{RioOutput, dump_frame};
    use crate::{
        RGBMatrix, RGBMatrixConfig,
        canvas::Canvas,
        config::K_BIT_PLANES,
        rp1::{self, Rp1Layout},
    };

    #[derive(Debug, PartialEq)]
    enum Op {
        Pins(u32),
        Clocked(u32),
        States(Vec<u32>),
        Hold(usize),
    }

    impl RioOutput for Vec<Op> {
        fn write_pins(&mut self, pins: u32) {
            self.push(Op::Pins(pins));
        }
        fn write_clocked_word(&mut self, pins: u32) {
            self.push(Op::Clocked(pins));
        }
        fn write_states(&mut self, states: &[u32]) {
            self.push(Op::States(states.to_vec()));
        }
        fn hold(&mut self, words: usize) {
            self.push(Op::Hold(words));
        }
    }

    /// Two columns and two double rows, showing only the highest bit plane.
    pub(crate) fn small_canvas() -> (RGBMatrixConfig, Canvas) {
        let mut config = RGBMatrixConfig {
            rows: 4,
            cols: 2,
            pwm_bits: 1,
            ..Default::default()
        };
        let map = RGBMatrix::create_pixel_map(&mut config).unwrap();
        let mut canvas = Canvas::new(&config, Arc::new(map));
        canvas.set_pixel(0, 0, 255, 0, 0);
        canvas.set_pixel(1, 3, 0, 0, 255);
        (config, canvas)
    }

    #[test]
    fn keeps_previous_bit_plane_on_while_clocking() {
        let (config, canvas) = small_canvas();
        let h = rp1::tests::regular_pins();
        let (oe, strobe, a, r1, b2) = (h.oe, h.strobe, h.a, h.r1, h.b2);
        let layout = Rp1Layout::new(&config, true).unwrap();
        let active_words = [3; K_BIT_PLANES];
        let mut pending = None;
        let mut ops = Vec::new();
        dump_frame(&layout, &active_words, &mut pending, &canvas, 0, &mut ops);
        assert_eq!(
            ops,
            [
                // Row 0 starts dark, still addressing the last row.
                Op::Clocked(r1 | a | oe),
                Op::Clocked(a | oe),
                Op::States(vec![oe, oe | strobe, oe]),
                // Row 0 is on while row 1 is clocked in, for 3 words.
                Op::Clocked(0),
                Op::Clocked(b2),
                Op::Pins(0),
                Op::Hold(1),
                Op::States(vec![a | oe, a | oe | strobe, a | oe]),
                Op::Pins(a | oe),
            ]
        );
        // The next frame shows the last row while clocking in the first.
        assert_eq!(pending, Some((a, 3)));
        ops.clear();
        dump_frame(&layout, &active_words, &mut pending, &canvas, 0, &mut ops);
        assert_eq!(ops[..2], [Op::Clocked(r1 | a), Op::Clocked(a)]);
    }
}
