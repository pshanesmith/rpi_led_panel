<p align="center">
    <img src="assets/logo.png" width="350">
</p>

[![Latest Version]][crates.io]

[latest version]: https://img.shields.io/crates/v/rpi_led_panel.svg
[crates.io]: https://crates.io/crates/rpi_led_panel

# 🦀 Raspberry Pi LED Panel

A Rust rewrite of the fantastic [rpi-rgb-led-matrix](https://github.com/hzeller/rpi-rgb-led-matrix) library. Use this if you want something that is easy to integrate into your Rust project but don't mind it being mostly untested. Contributions are welcome.

See the [examples](examples) to get started. You can run any of them with `--help` to get a list of command line arguments. See the [original readme](https://github.com/hzeller/rpi-rgb-led-matrix) for more information.

## Raspberry Pi 5

On a Raspberry Pi 5, the GPIO pins are driven through its RP1 I/O controller, which is detected automatically. By default, the CPU sets the pins directly through RP1's registered I/O block, which works without root privileges through `/dev/gpiomem0`. With `--rp1-pio true`, a PIO state machine of the RP1 clocks out the data instead, which needs the kernel's PIO driver at `/dev/pio0`. On the Raspberry Pi 5, `--slowdown` divides the pixel clock.

Like upstream, the Raspberry Pi 5 supports the `Regular`, `Classic`, `AdafruitHat` and `AdafruitHatPwm` hardware mappings (and `RegularPi1` with PIO), up to 3 parallel chains, and no GPIO inputs or outputs for the user.

## Compute Module

The `ComputeModule` hardware mapping supports up to 3 parallel chains. Enable the `wide-gpio` feature to use all 6, which needs the GPIO pins above 31 and doubles the memory per canvas.
