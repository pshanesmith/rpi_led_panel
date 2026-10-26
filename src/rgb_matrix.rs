use std::{
    error::Error,
    fmt::{Display, Formatter},
    fs::{OpenOptions, write},
    io::Write,
    mem::replace,
    sync::Arc,
    sync::atomic::{AtomicU32, Ordering},
    sync::mpsc::{
        Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, TrySendError, channel,
        sync_channel,
    },
    thread::{JoinHandle, sleep, spawn},
    time::{Duration, Instant},
};
use thread_priority::{ThreadPriority, set_current_thread_priority};

use crate::{
    RGBMatrixConfig,
    backend::Backend,
    canvas::{Canvas, PixelDesignator, PixelDesignatorMap},
    chip::PiChip,
    config::K_BIT_PLANES,
    gpio::GpioInitializationError,
    named_pixel_mapper::MappingDirection,
    pixel_mapper::PixelMapper,
    registers::{STORE_BARRIER_SUPPORTED, UserGpioOutputs},
    row_address_setter::RowAddressSetterType,
    rp1::is_pi5_model,
    utils::{FrameRateMonitor, linux_has_isol_cpu, set_thread_affinity, sleep_at_most},
};

/// How many input changes are kept until they are received.
const INPUT_QUEUE_LENGTH: usize = 256;

fn initialize_update_thread(chip: PiChip) {
    // Pin the thread to the last core to avoid the flicker resulting from context switching.
    let last_core_id = chip.num_cores() - 1;
    if !set_thread_affinity(last_core_id) {
        eprintln!("Could not pin the display update thread to core {last_core_id}.");
    }

    // If the user has not setup isolcpus, let them know about the performance improvement.
    if chip.num_cores() > 1 && !linux_has_isol_cpu(last_core_id) {
        eprintln!(
            "Suggestion: to slightly improve display update, add\n\tisolcpus={last_core_id}\nat \
            the end of /boot/cmdline.txt and reboot"
        );
    }

    // Disable realtime throttling.
    if chip.num_cores() > 1 && write("/proc/sys/kernel/sched_rt_runtime_us", "999000").is_err() {
        eprintln!("Could not disable realtime throttling");
    }

    // Set the last core to performance mode.
    if chip.num_cores() > 1
        && write(
            format!("/sys/devices/system/cpu/cpu{last_core_id}/cpufreq/scaling_governor"),
            "performance",
        )
        .is_err()
    {
        eprintln!(
            "Could not set core {} to performance mode.",
            last_core_id + 1
        );
    }

    // Set the highest thread priority.
    if set_current_thread_priority(ThreadPriority::Max).is_err() {
        eprintln!("Could not set thread priority. This might lead to reduced performance.",);
    }
}

#[derive(Debug)]
pub enum MatrixCreationError {
    ChipDeterminationError,
    TooManyParallelChains(usize),
    InvalidDitherBits(usize),
    ThreadTimedOut,
    ThreadPanicked(String),
    InvalidConfig(String),
    GpioError(GpioInitializationError),
    MemoryAccessError,
    PixelMapperError(String),
}

impl Error for MatrixCreationError {}

impl Display for MatrixCreationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            MatrixCreationError::ChipDeterminationError => {
                f.write_str("Failed to automatically determine Raspberry Pi model.")
            }
            MatrixCreationError::TooManyParallelChains(max) => {
                write!(f, "GPIO mapping only supports up to {max} parallel panels.")
            }
            MatrixCreationError::InvalidDitherBits(value) => {
                write!(f, "Unsupported dither bits '{value}'.")
            }
            MatrixCreationError::ThreadTimedOut => {
                f.write_str("The update thread did not return in time.")
            }
            MatrixCreationError::ThreadPanicked(message) => {
                write!(
                    f,
                    "The update thread failed during initialization: {message}"
                )
            }
            MatrixCreationError::InvalidConfig(message) => {
                write!(f, "Invalid configuration: {message}")
            }
            MatrixCreationError::GpioError(error) => {
                write!(f, "GPIO initialization error: {error}")
            }
            MatrixCreationError::MemoryAccessError => f.write_str(
                "Failed to access the physical memory. Not running with root privileges?",
            ),
            MatrixCreationError::PixelMapperError(message) => {
                write!(f, "Error in pixel mapper: {message}")
            }
        }
    }
}

/// Requests from the user to the display update thread.
enum Control {
    Shutdown,
    /// Turns the pins into outputs and replies with the ones that were available.
    RequestOutputs(u32, Sender<u32>),
}

pub struct RGBMatrix {
    /// The join handle of the update thread.
    thread_handle: Option<JoinHandle<()>>,
    /// Requests to the update thread.
    control_sender: Sender<Control>,
    /// Receiver for GPIO inputs.
    input_receiver: Receiver<u32>,
    /// Channel to send canvas to update thread, with the number of frames to show the previous canvas for.
    canvas_to_thread_sender: SyncSender<(Box<Canvas>, u32)>,
    /// Channel to receive canvas from update thread.
    canvas_from_thread_receiver: Receiver<Box<Canvas>>,
    /// Additional requested inputs that can be received.
    enabled_input_bits: u32,
    /// The registers to write the outputs of the user. Not available on the Raspberry Pi 5.
    user_outputs: Option<UserGpioOutputs>,
    /// The pins that the user can write to.
    enabled_output_bits: u32,
    /// Frame rate measurement.
    framerate: Arc<AtomicU32>,
}

impl RGBMatrix {
    /// Create a new RGB matrix controller. This starts a new thread to update the matrix. Returns the
    /// controller and a canvas for drawing.
    ///
    /// You can additionally request user readable GPIO bits which can later be received with
    /// [`RGBMatrix::receive_new_inputs`]. Only bits that are not already in use for reading or writing by the
    /// matrix are allowed. Use [`RGBMatrix::enabled_input_bits`] after calling this function to check which
    /// bits were actually available. GPIO inputs are not supported on the Raspberry Pi 5.
    ///
    /// Note: If privilege dropping is enabled in the config, then calling this function will have the side
    /// effect of dropping the application's privileges down to the configured user/group.
    pub fn new(
        mut config: RGBMatrixConfig,
        requested_inputs: u32,
    ) -> Result<(Self, Box<Canvas>), MatrixCreationError> {
        let chip = if let Some(chip) = config.pi_chip {
            chip
        } else {
            PiChip::determine().ok_or(MatrixCreationError::ChipDeterminationError)?
        };
        let uses_rp1 = chip.uses_rp1() || is_pi5_model();

        // Check if we can access the memory before doing anything else. The Raspberry Pi 5 doesn't need it.
        if !uses_rp1 {
            OpenOptions::new()
                .read(true)
                .write(true)
                .open("/dev/mem")
                .map_err(|_| MatrixCreationError::MemoryAccessError)?;
        }

        let max_parallel = config.hardware_mapping.max_parallel_chains();
        if config.parallel > max_parallel {
            return Err(MatrixCreationError::TooManyParallelChains(max_parallel));
        }
        Self::validate_config(&config)?;

        let shared_mapper = Self::create_pixel_map(&mut config)?;

        let dither_start_bits = match config.dither_bits {
            0 => [0, 0, 0, 0],
            1 => [0, 1, 0, 1],
            2 => [0, 1, 2, 2],
            _ => return Err(MatrixCreationError::InvalidDitherBits(config.dither_bits)),
        };

        // Create two canvases, one for the display update thread and one for the user to modify. They will be
        // swapped out after each frame.
        let canvas = Box::new(Canvas::new(&config, Arc::new(shared_mapper)));
        let mut thread_canvas = canvas.clone();

        // The update thread drops the privileges, so map the registers for the user's outputs before.
        let user_outputs = (!uses_rp1).then(|| UserGpioOutputs::new(chip));

        let (canvas_to_thread_sender, canvas_to_thread_receiver) =
            sync_channel::<(Box<Canvas>, u32)>(0);
        let (canvas_from_thread_sender, canvas_from_thread_receiver) =
            sync_channel::<Box<Canvas>>(1);
        let (control_sender, control_receiver) = channel::<Control>();
        let (input_sender, input_receiver) = sync_channel::<u32>(INPUT_QUEUE_LENGTH);
        let (thread_start_result_sender, thread_start_result_receiver) =
            channel::<Result<u32, MatrixCreationError>>();
        let framerate = Arc::new(AtomicU32::new(0));
        let mut frame_rate_monitor = FrameRateMonitor::new(framerate.clone());

        let thread_handle = spawn(move || {
            initialize_update_thread(chip);

            let mut backend = match Backend::new(chip, &config) {
                Ok(backend) => backend,
                Err(error) => {
                    // If sending fails, the main thread has already given up waiting.
                    thread_start_result_sender.send(Err(error)).ok();
                    return;
                }
            };

            // Run the initialization sequence if necessary.
            if let Some(panel_type) = config.panel_type
                && let Err(error) = backend.init_panels(&panel_type.init(&config))
            {
                thread_start_result_sender.send(Err(error)).ok();
                return;
            }

            let mut last_gpio_inputs: u32 = 0;

            // Dither sequence
            let mut dither_low_bit_sequence = 0;

            let frame_time_target = Duration::from_secs(1) / config.refresh_rate as u32;

            // The canvas to show next, and for how many frames to show the current one before.
            let mut next_canvas: Option<Box<Canvas>> = None;
            let mut frame_multiple = 1;
            let mut frame_count: u32 = 0;
            let mut refresh_display = config.show_refresh.then(RefreshRateDisplay::new);

            let enabled_input_bits = backend.request_inputs(requested_inputs);
            if thread_start_result_sender
                .send(Ok(enabled_input_bits))
                .is_err()
            {
                // The main thread has already given up waiting.
                return;
            }

            'thread: loop {
                let start_time = Instant::now();
                loop {
                    match control_receiver.try_recv() {
                        Ok(Control::RequestOutputs(bits, reply)) => {
                            reply.send(backend.request_outputs(bits)).ok();
                        }
                        Ok(Control::Shutdown) | Err(TryRecvError::Disconnected) => break 'thread,
                        Err(TryRecvError::Empty) => break,
                    }
                }
                // Read input bits and send them if they have changed.
                let new_inputs = backend.read_inputs();
                if new_inputs != last_gpio_inputs {
                    match input_sender.try_send(new_inputs) {
                        Ok(()) => last_gpio_inputs = new_inputs,
                        // Nobody is receiving the inputs. Try again in the next frame.
                        Err(TrySendError::Full(_)) => {}
                        Err(TrySendError::Disconnected(_)) => break 'thread,
                    }
                }
                // Check for a swap canvas.
                if next_canvas.is_none() {
                    match canvas_to_thread_receiver.try_recv() {
                        Ok((new_canvas, multiple)) => {
                            next_canvas = Some(new_canvas);
                            frame_multiple = multiple.max(1);
                        }
                        Err(TryRecvError::Disconnected) => {
                            break 'thread;
                        }
                        Err(TryRecvError::Empty) => {}
                    }
                }
                // Only swap after the requested number of frames.
                if frame_count.is_multiple_of(frame_multiple) {
                    frame_count = 0;
                    if let Some(new_canvas) = next_canvas.take() {
                        let old_canvas = replace(&mut thread_canvas, new_canvas);
                        let Ok(()) = canvas_from_thread_sender.send(old_canvas) else {
                            break 'thread;
                        };
                    }
                }

                backend.dump(
                    &thread_canvas,
                    dither_start_bits[dither_low_bit_sequence % dither_start_bits.len()],
                );
                dither_low_bit_sequence += 1;
                frame_count += 1;

                frame_rate_monitor.update();

                // Wait for the rest of the frame.
                let end_time = start_time + frame_time_target;
                if config.busy_waiting {
                    sleep_at_most(end_time.saturating_duration_since(Instant::now()));
                    while Instant::now() < end_time {
                        std::hint::spin_loop();
                    }
                } else {
                    sleep(end_time.saturating_duration_since(Instant::now()));
                }

                if let Some(display) = refresh_display.as_mut() {
                    display.update(start_time.elapsed());
                }
            }

            // Turn it off.
            thread_canvas.fill(0, 0, 0);
            backend.dump(&thread_canvas, 0);
        });

        let enabled_input_bits =
            match thread_start_result_receiver.recv_timeout(Duration::from_secs(10)) {
                Ok(result) => result?,
                Err(RecvTimeoutError::Timeout) => return Err(MatrixCreationError::ThreadTimedOut),
                Err(RecvTimeoutError::Disconnected) => {
                    // The thread exited without reporting back, so it must have panicked.
                    let message = match thread_handle.join() {
                        Err(payload) => payload
                            .downcast_ref::<&str>()
                            .map(ToString::to_string)
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "unknown panic".to_string()),
                        Ok(()) => "thread exited unexpectedly".to_string(),
                    };
                    return Err(MatrixCreationError::ThreadPanicked(message));
                }
            };

        let rgbmatrix = Self {
            thread_handle: Some(thread_handle),
            control_sender,
            input_receiver,
            canvas_to_thread_sender,
            canvas_from_thread_receiver,
            enabled_input_bits,
            user_outputs,
            enabled_output_bits: 0,
            framerate,
        };

        Ok((rgbmatrix, canvas))
    }

    /// Creates the map from visible pixels to the bit plane buffer. Multiplexers might change the physical
    /// layout, so this updates the rows and columns in the config before the hardware is set up.
    pub(crate) fn create_pixel_map(
        config: &mut RGBMatrixConfig,
    ) -> Result<PixelDesignatorMap, MatrixCreationError> {
        let multiplex_mapper = config.multiplexing.as_ref().map(|mapper_type| {
            let mut mapper = mapper_type.create();
            mapper.edit_rows_cols(&mut config.rows, &mut config.cols);
            mapper
        });
        // The multiplexer changes the electrical layout, which has to be supported as well.
        Self::validate_rows(config.rows)?;

        let pixel_designator = PixelDesignator::new(&config.hardware_mapping, config.led_sequence);
        let matrix = PixelDesignatorMap::new(pixel_designator, config);
        let mut shared_mapper = matrix.clone();

        // Apply the mapping for the panels first.
        let multiplex_mapper_applied = multiplex_mapper.is_some();
        if let Some(mapper) = multiplex_mapper {
            let mapper = PixelMapper::Multiplex(mapper);
            shared_mapper = Self::apply_pixel_mapper(&shared_mapper, &mapper, pixel_designator)?;
        }

        // Apply higher level mappers that might arrange panels.
        for mapper_type in config.pixelmapper.iter() {
            let mapper = mapper_type.create(config.chain_length, config.parallel)?;
            let mapper = PixelMapper::Named(mapper);
            shared_mapper = Self::apply_pixel_mapper(&shared_mapper, &mapper, pixel_designator)?;
        }
        if multiplex_mapper_applied || !config.pixelmapper.is_empty() {
            shared_mapper.find_hidden_bits(&matrix);
        }
        Ok(shared_mapper)
    }

    /// Rejects configurations that would otherwise lead to panics or a blank display.
    fn validate_config(config: &RGBMatrixConfig) -> Result<(), MatrixCreationError> {
        let invalid = |message: String| Err(MatrixCreationError::InvalidConfig(message));
        if config.cols == 0 || config.chain_length == 0 || config.parallel == 0 {
            return invalid(format!(
                "cols ({}), chain_length ({}) and parallel ({}) must be at least 1.",
                config.cols, config.chain_length, config.parallel
            ));
        }
        Self::validate_rows(config.rows)?;
        if !(1..=K_BIT_PLANES).contains(&config.pwm_bits) {
            return invalid(format!(
                "pwm_bits must be between 1 and {K_BIT_PLANES}, got {}.",
                config.pwm_bits
            ));
        }
        // The PWM clock divider is `pwm_lsb_nanoseconds / 4` and needs to fit in 12 bits.
        if !(4..16384).contains(&config.pwm_lsb_nanoseconds) {
            return invalid(format!(
                "pwm_lsb_nanoseconds must be between 4 and 16383, got {}.",
                config.pwm_lsb_nanoseconds
            ));
        }
        if config.refresh_rate == 0 {
            return invalid("refresh_rate must be at least 1.".to_string());
        }
        if let Some(slowdown) = config.slowdown {
            if !(-1..=60).contains(&slowdown) {
                return invalid(format!(
                    "slowdown must be between -1 and 60, got {slowdown}."
                ));
            }
            if slowdown == -1 && !STORE_BARRIER_SUPPORTED {
                return invalid("slowdown -1 needs an ARMv7 or newer processor.".to_string());
            }
        }
        // The B707 shift register advances by one row with every address, so the rows have to be in order.
        if config.interlaced && config.row_setter == RowAddressSetterType::B707ShiftRegister {
            return invalid(
                "The B707ShiftRegister row setter does not support interlacing.".to_string(),
            );
        }
        Ok(())
    }

    /// The row address setters support up to 32 double rows.
    fn validate_rows(rows: usize) -> Result<(), MatrixCreationError> {
        if !(2..=64).contains(&rows) || !rows.is_multiple_of(2) {
            return Err(MatrixCreationError::InvalidConfig(format!(
                "rows must be an even number between 2 and 64, got {rows}."
            )));
        }
        Ok(())
    }

    fn apply_pixel_mapper(
        shared_mapper: &PixelDesignatorMap,
        mapper: &PixelMapper,
        pixel_designator: PixelDesignator,
    ) -> Result<PixelDesignatorMap, MatrixCreationError> {
        let old_width = shared_mapper.width();
        let old_height = shared_mapper.height();
        let [new_width, new_height] = mapper.get_size_mapping(old_width, old_height)?;
        if new_width == 0 || new_height == 0 {
            return Err(MatrixCreationError::PixelMapperError(format!(
                "The pixel mapper turns the {old_width}x{old_height} matrix into an empty canvas."
            )));
        }
        let mut new_mapper = PixelDesignatorMap::blank(pixel_designator, new_width, new_height);
        let invalid_dimensions = || {
            MatrixCreationError::PixelMapperError(
                "Invalid dimensions detected. This is likely a bug.".to_string(),
            )
        };
        match mapper.mapping_direction() {
            MappingDirection::VisibleToMatrix => {
                for y in 0..new_height {
                    for x in 0..new_width {
                        let [orig_x, orig_y] =
                            mapper.map_visible_to_matrix(old_width, old_height, x, y);
                        let orig_designator = shared_mapper
                            .get(orig_x, orig_y)
                            .ok_or_else(invalid_dimensions)?;
                        *new_mapper.get_mut(x, y).unwrap() = *orig_designator;
                    }
                }
            }
            MappingDirection::MatrixToVisible => {
                let mut collision_reported = false;
                for y in 0..old_height {
                    for x in 0..old_width {
                        let Some([new_x, new_y]) =
                            mapper.map_matrix_to_visible(old_width, old_height, x, y)
                        else {
                            continue;
                        };
                        let new_designator = new_mapper
                            .get_mut(new_x, new_y)
                            .ok_or_else(invalid_dimensions)?;
                        if new_designator.is_used() && !collision_reported {
                            eprintln!(
                                "Warning: pixel mapper maps more than one pixel to ({new_x}, {new_y})."
                            );
                            collision_reported = true;
                        }
                        *new_designator = *shared_mapper.get(x, y).unwrap();
                    }
                }
            }
        }
        Ok(new_mapper)
    }

    /// Updates the matrix with the new canvas. Blocks until the end of the current frame.
    /// This returns a cleared canvas that has to be drawn to before calling this function again.
    /// If this function is not called before the next frame update, the previous
    /// frame is drawn again.
    ///
    /// The returned canvas has the same brightness, PWM bits and luminance correction as the one passed in.
    pub fn update_on_vsync(&mut self, canvas: Box<Canvas>) -> Box<Canvas> {
        self.update_on_vsync_every(canvas, 1)
    }

    /// Like [`RGBMatrix::update_on_vsync`], but the current canvas is shown for a multiple of `frames` frames
    /// before the new one. This gives a stable frame rate that is a fraction of the refresh rate, e.g. for
    /// animations.
    pub fn update_on_vsync_every(&mut self, canvas: Box<Canvas>, frames: u32) -> Box<Canvas> {
        let Self {
            canvas_to_thread_sender,
            canvas_from_thread_receiver,
            ..
        } = self;

        let brightness = canvas.brightness();
        let pwm_bits = canvas.pwm_bits();
        let luminance_correct = canvas.luminance_correct();

        canvas_to_thread_sender
            .send((canvas, frames))
            .expect("Display update thread shut down unexpectedly.");

        let mut canvas = canvas_from_thread_receiver
            .recv()
            .expect("Display update thread shut down unexpectedly.");

        // Both canvases need the same settings, otherwise they would alternate between frames.
        canvas.set_brightness(brightness);
        canvas.set_pwm_bits(pwm_bits);
        canvas.set_luminance_correct(luminance_correct);

        // Clear the canvas before handing it back to the user.
        // There is no practical use for not clearing it here as we switch
        // back and forth between two canvases.
        canvas.fill(0, 0, 0);
        canvas
    }

    /// Get the bits that were available for input.
    #[must_use]
    pub fn enabled_input_bits(&self) -> u32 {
        self.enabled_input_bits
    }

    /// Tries to receive a new GPIO input as requested in [`RGBMatrix::new`].
    pub fn receive_new_inputs(&mut self, timeout: Duration) -> Option<u32> {
        self.input_receiver.recv_timeout(timeout).ok()
    }

    /// Turns GPIO pins that are not used by the matrix into outputs, which can be set with
    /// [`RGBMatrix::output_gpio`]. Returns the pins that were available. GPIO outputs are not supported on the
    /// Raspberry Pi 5.
    pub fn request_outputs(&mut self, bits: u32) -> u32 {
        if self.user_outputs.is_none() {
            return 0;
        }
        let (reply_sender, reply_receiver) = channel();
        if self
            .control_sender
            .send(Control::RequestOutputs(bits, reply_sender))
            .is_err()
        {
            return 0;
        }
        let enabled = reply_receiver.recv().unwrap_or(0);
        self.enabled_output_bits |= enabled;
        enabled
    }

    /// Sets the outputs from [`RGBMatrix::request_outputs`] to the given values. Other bits are ignored.
    pub fn output_gpio(&self, values: u32) {
        if let Some(outputs) = &self.user_outputs {
            outputs.write_masked_bits(values, self.enabled_output_bits);
        }
    }

    /// Get the average frame rate over the last 60 frames.
    #[must_use]
    pub fn get_framerate(&self) -> usize {
        self.framerate.load(Ordering::Relaxed) as usize
    }
}

impl Drop for RGBMatrix {
    fn drop(&mut self) {
        let Self {
            thread_handle,
            control_sender,
            ..
        } = self;
        if let Some(handle) = thread_handle.take() {
            control_sender.send(Control::Shutdown).ok();
            let _result = handle.join();
        }
    }
}

/// Prints the refresh rate of each frame, and the lowest one after the start up.
struct RefreshRateDisplay {
    start: Instant,
    longest_frame: Duration,
}

impl RefreshRateDisplay {
    /// Times are janky at the start, so the lowest refresh rate is only measured after this.
    const HOLDOFF: Duration = Duration::from_secs(2);

    fn new() -> Self {
        Self {
            start: Instant::now(),
            longest_frame: Duration::ZERO,
        }
    }

    fn update(&mut self, frame_time: Duration) {
        let mut stdout = std::io::stdout().lock();
        let hz = 1.0 / frame_time.as_secs_f64();
        let _ = write!(stdout, "\r{hz:6.1}Hz");
        if self.start.elapsed() > Self::HOLDOFF && frame_time > self.longest_frame {
            self.longest_frame = frame_time;
        }
        if !self.longest_frame.is_zero() {
            let lowest = 1.0 / self.longest_frame.as_secs_f64();
            let _ = write!(stdout, " (lowest: {lowest:.1}Hz)");
        }
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::RGBMatrix;
    use crate::{MultiplexMapperType, NamedPixelMapperType, RGBMatrixConfig};

    #[test]
    fn rotates_long_chain() {
        let mut config = RGBMatrixConfig {
            rows: 32,
            cols: 64,
            chain_length: 4,
            pixelmapper: vec![NamedPixelMapperType::Rotate(90)],
            ..Default::default()
        };
        let map = RGBMatrix::create_pixel_map(&mut config).unwrap();
        assert_eq!((map.width(), map.height()), (32, 256));
    }

    #[test]
    fn multiplexes_parallel_chains() {
        let mut config = RGBMatrixConfig {
            rows: 16,
            cols: 32,
            parallel: 2,
            multiplexing: Some(MultiplexMapperType::Kaler2Scan),
            ..Default::default()
        };
        let map = RGBMatrix::create_pixel_map(&mut config).unwrap();
        assert_eq!((map.width(), map.height()), (32, 32));
    }

    #[test]
    fn remaps_every_pixel_once() {
        let mut config = RGBMatrixConfig {
            rows: 16,
            cols: 32,
            chain_length: 2,
            pixelmapper: vec!["Remap:32,32|0,0n|0,16s".parse().unwrap()],
            ..Default::default()
        };
        let map = RGBMatrix::create_pixel_map(&mut config).unwrap();
        assert_eq!((map.width(), map.height()), (32, 32));
        for y in 0..32 {
            for x in 0..32 {
                assert!(map.get(x, y).unwrap().is_used(), "({x}, {y}) is not mapped");
            }
        }
    }

    #[test]
    fn creates_maps_for_new_multiplexers() {
        for (multiplexing, rows, cols) in [
            (MultiplexMapperType::P10Outdoor32x16QuarterScan, 16, 32),
            (MultiplexMapperType::P3Outdoor64x64, 64, 64),
            (MultiplexMapperType::DoubleZ, 32, 64),
            (MultiplexMapperType::P4Outdoor80x40, 40, 80),
            (MultiplexMapperType::ZStripe44For40x20, 20, 40),
            (MultiplexMapperType::P10Outdoor1R1G1B2For40x20, 20, 40),
        ] {
            let mut config = RGBMatrixConfig {
                rows,
                cols,
                multiplexing: Some(multiplexing),
                ..Default::default()
            };
            assert!(
                RGBMatrix::create_pixel_map(&mut config).is_ok(),
                "{multiplexing:?}"
            );
        }
    }

    #[test]
    fn rejects_invalid_config() {
        let valid = RGBMatrixConfig::default;
        assert!(RGBMatrix::validate_config(&valid()).is_ok());
        for config in [
            RGBMatrixConfig {
                pwm_bits: 0,
                ..valid()
            },
            RGBMatrixConfig {
                pwm_bits: 12,
                ..valid()
            },
            RGBMatrixConfig {
                rows: 128,
                ..valid()
            },
            RGBMatrixConfig {
                rows: 33,
                ..valid()
            },
            RGBMatrixConfig {
                parallel: 0,
                ..valid()
            },
            RGBMatrixConfig {
                chain_length: 0,
                ..valid()
            },
            RGBMatrixConfig {
                refresh_rate: 0,
                ..valid()
            },
            RGBMatrixConfig {
                pwm_lsb_nanoseconds: 0,
                ..valid()
            },
            RGBMatrixConfig {
                interlaced: true,
                row_setter: crate::RowAddressSetterType::B707ShiftRegister,
                ..valid()
            },
        ] {
            assert!(RGBMatrix::validate_config(&config).is_err(), "{config:?}");
        }
    }
}
