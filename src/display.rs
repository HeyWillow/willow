//! SPI2 and ST7789 display panel ownership.
//!
//! Rust retains the panel and IO resources for the firmware lifetime. The
//! Rust UI borrows their raw ESP-IDF handles without taking ownership.

use core::ptr;
use std::sync::OnceLock;

use esp_idf_sys::{
    ESP_ERR_INVALID_ARG, ESP_ERR_INVALID_STATE, EspError, esp_err_t, esp_lcd_new_panel_io_spi,
    esp_lcd_new_panel_st7789, esp_lcd_panel_del, esp_lcd_panel_dev_config_t,
    esp_lcd_panel_disp_on_off, esp_lcd_panel_handle_t, esp_lcd_panel_init,
    esp_lcd_panel_invert_color, esp_lcd_panel_io_del, esp_lcd_panel_io_handle_t,
    esp_lcd_panel_io_spi_config_t, esp_lcd_panel_mirror, esp_lcd_panel_reset,
    esp_lcd_panel_set_gap, esp_lcd_panel_swap_xy, esp_lcd_spi_bus_handle_t, gpio_config,
    gpio_config_t, gpio_mode_t_GPIO_MODE_OUTPUT, gpio_num_t_GPIO_NUM_4, gpio_num_t_GPIO_NUM_5,
    gpio_num_t_GPIO_NUM_6, gpio_num_t_GPIO_NUM_7, gpio_num_t_GPIO_NUM_45, gpio_num_t_GPIO_NUM_47,
    gpio_num_t_GPIO_NUM_48, lcd_rgb_element_order_t_LCD_RGB_ELEMENT_ORDER_BGR,
    lcd_rgb_element_order_t_LCD_RGB_ELEMENT_ORDER_RGB, spi_bus_config_t, spi_bus_free,
    spi_bus_initialize, spi_common_dma_t_SPI_DMA_CH_AUTO, spi_host_device_t_SPI2_HOST,
};
use log::{debug, error};

const BITS_PER_PIXEL: u32 = 16;
const COMMAND_BITS: i32 = 8;
const HORIZONTAL_RESOLUTION: i32 = 320;
const LOG_TARGET: &str = "WILLOW/DISPLAY";
const PARAMETER_BITS: i32 = 8;
const PIXEL_CLOCK_HZ: u32 = 10 * 1_000 * 1_000;
const TRANSACTION_QUEUE_DEPTH: usize = 10;
const VERTICAL_RESOLUTION: i32 = 240;

#[derive(Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "these independent flags directly describe fixed panel wiring"
)]
struct BoardConfiguration {
    backlight_gpio: i32,
    backlight_on_level: u32,
    color_order: u32,
    invert_color: bool,
    max_transfer_size: i32,
    mirror_x: bool,
    mirror_y: bool,
    reset_active_high: bool,
    swap_xy: bool,
}

const BOARD: BoardConfiguration = if cfg!(esp_idf_esp32_s3_box_lite_board) {
    BoardConfiguration {
        backlight_gpio: gpio_num_t_GPIO_NUM_45,
        backlight_on_level: 0,
        color_order: lcd_rgb_element_order_t_LCD_RGB_ELEMENT_ORDER_RGB,
        invert_color: true,
        max_transfer_size: 16 * HORIZONTAL_RESOLUTION * 2 + 8,
        mirror_x: false,
        mirror_y: true,
        reset_active_high: false,
        swap_xy: true,
    }
} else {
    BoardConfiguration {
        backlight_gpio: if cfg!(esp_idf_esp32_s3_box_3_board) {
            gpio_num_t_GPIO_NUM_47
        } else {
            gpio_num_t_GPIO_NUM_45
        },
        backlight_on_level: 1,
        color_order: lcd_rgb_element_order_t_LCD_RGB_ELEMENT_ORDER_BGR,
        invert_color: false,
        max_transfer_size: HORIZONTAL_RESOLUTION * VERTICAL_RESOLUTION * 2,
        mirror_x: true,
        mirror_y: true,
        reset_active_high: cfg!(esp_idf_esp32_s3_box_3_board),
        swap_xy: false,
    }
};

struct Display {
    io: usize,
    panel: usize,
}

impl Display {
    fn io(&self) -> esp_lcd_panel_io_handle_t {
        self.io as esp_lcd_panel_io_handle_t
    }

    fn panel(&self) -> esp_lcd_panel_handle_t {
        self.panel as esp_lcd_panel_handle_t
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        unsafe {
            if !self.panel().is_null() {
                let _ = esp_lcd_panel_del(self.panel());
            }
            if !self.io().is_null() {
                let _ = esp_lcd_panel_io_del(self.io());
            }
            let _ = spi_bus_free(spi_host_device_t_SPI2_HOST);
        }
    }
}

static DISPLAY: OnceLock<Display> = OnceLock::new();

pub(crate) fn handles() -> Option<(esp_lcd_panel_io_handle_t, esp_lcd_panel_handle_t)> {
    DISPLAY.get().map(|display| (display.io(), display.panel()))
}

fn check(result: esp_err_t, operation: &str) -> Result<(), EspError> {
    if let Some(error) = EspError::from(result) {
        error!(target: LOG_TARGET, "{operation}: {error}");
        Err(error)
    } else {
        Ok(())
    }
}

fn initialize_backlight_gpio() -> Result<(), EspError> {
    let configuration = gpio_config_t {
        mode: gpio_mode_t_GPIO_MODE_OUTPUT,
        pin_bit_mask: 1_u64 << BOARD.backlight_gpio,
        ..Default::default()
    };
    check(
        unsafe { gpio_config(&raw const configuration) },
        "failed to configure initial backlight GPIO",
    )?;
    check(
        unsafe { esp_idf_sys::gpio_set_level(BOARD.backlight_gpio, BOARD.backlight_on_level) },
        "failed to enable initial display backlight",
    )
}

fn spi_bus_configuration() -> spi_bus_config_t {
    let mut configuration = spi_bus_config_t::default();
    configuration.__bindgen_anon_1.mosi_io_num = gpio_num_t_GPIO_NUM_6;
    configuration.__bindgen_anon_2.miso_io_num = -1;
    configuration.__bindgen_anon_3.quadwp_io_num = -1;
    configuration.__bindgen_anon_4.quadhd_io_num = -1;
    configuration.sclk_io_num = gpio_num_t_GPIO_NUM_7;
    configuration.max_transfer_sz = BOARD.max_transfer_size;
    configuration
}

fn panel_io_configuration() -> esp_lcd_panel_io_spi_config_t {
    esp_lcd_panel_io_spi_config_t {
        cs_gpio_num: gpio_num_t_GPIO_NUM_5,
        dc_gpio_num: gpio_num_t_GPIO_NUM_4,
        lcd_cmd_bits: COMMAND_BITS,
        lcd_param_bits: PARAMETER_BITS,
        on_color_trans_done: None,
        pclk_hz: PIXEL_CLOCK_HZ,
        spi_mode: 0,
        trans_queue_depth: TRANSACTION_QUEUE_DEPTH,
        user_ctx: ptr::null_mut(),
        ..Default::default()
    }
}

fn panel_configuration() -> esp_lcd_panel_dev_config_t {
    let mut configuration = esp_lcd_panel_dev_config_t {
        reset_gpio_num: gpio_num_t_GPIO_NUM_48,
        bits_per_pixel: BITS_PER_PIXEL,
        ..Default::default()
    };
    configuration.__bindgen_anon_1.rgb_ele_order = BOARD.color_order;
    configuration
        .flags
        .set_reset_active_high(u32::from(BOARD.reset_active_high));
    configuration
}

/// Initializes and retains the SPI bus, panel IO, ST7789 panel, and backlight.
pub(crate) fn initialize() -> Result<(), EspError> {
    if DISPLAY.get().is_some() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    debug!(target: LOG_TARGET, "initializing display panel in Rust");
    initialize_backlight_gpio()?;

    let bus_configuration = spi_bus_configuration();
    check(
        unsafe {
            spi_bus_initialize(
                spi_host_device_t_SPI2_HOST,
                &raw const bus_configuration,
                spi_common_dma_t_SPI_DMA_CH_AUTO,
            )
        },
        "failed to initialize display SPI bus",
    )?;

    let mut display = Display { io: 0, panel: 0 };
    let io_configuration = panel_io_configuration();
    let mut io = ptr::null_mut();
    let bus_handle: esp_lcd_spi_bus_handle_t = i32::try_from(spi_host_device_t_SPI2_HOST)
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_ARG>())?;
    check(
        unsafe { esp_lcd_new_panel_io_spi(bus_handle, &raw const io_configuration, &raw mut io) },
        "failed to create display panel IO",
    )?;
    display.io = io as usize;

    let panel_configuration = panel_configuration();
    let mut panel = ptr::null_mut();
    check(
        unsafe { esp_lcd_new_panel_st7789(io, &raw const panel_configuration, &raw mut panel) },
        "failed to create ST7789 display panel",
    )?;
    display.panel = panel as usize;

    check(
        unsafe { esp_lcd_panel_reset(panel) },
        "failed to reset display panel",
    )?;
    check(
        unsafe { esp_lcd_panel_init(panel) },
        "failed to initialize display panel",
    )?;
    check(
        unsafe { esp_lcd_panel_invert_color(panel, BOARD.invert_color) },
        "failed to configure display color inversion",
    )?;
    check(
        unsafe { esp_lcd_panel_set_gap(panel, 0, 0) },
        "failed to configure display panel gap",
    )?;
    check(
        unsafe { esp_lcd_panel_swap_xy(panel, BOARD.swap_xy) },
        "failed to configure display axis order",
    )?;
    check(
        unsafe { esp_lcd_panel_mirror(panel, BOARD.mirror_x, BOARD.mirror_y) },
        "failed to configure display mirroring",
    )?;
    check(
        unsafe { esp_lcd_panel_disp_on_off(panel, true) },
        "failed to turn on display panel",
    )?;

    crate::backlight::initialize()?;
    DISPLAY
        .set(display)
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())
}
