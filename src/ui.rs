//! Rust-owned Willow UI rendered with `embedded-graphics`.
//!
//! Normal Rust functions expose semantic UI transitions. Temporary C exports
//! at the bottom of this module only convert ABI values and delegate to those
//! functions. A dedicated Rust task snapshots the state, renders changed bands
//! into an RGB565 frame in PSRAM, and flushes them through a small internal DMA
//! buffer. Bounded oversized lines are cached in PSRAM and only their viewport
//! is flushed while scrolling. Touch polling is also Rust-owned and sends
//! nonblocking cancellation through the Rust audio APIs.

use core::{convert::Infallible, ffi::c_char, mem::size_of, ptr, slice};
use std::{
    borrow::Cow,
    ffi::CStr,
    ptr::NonNull,
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use embedded_graphics::{
    Drawable,
    draw_target::DrawTarget,
    geometry::{Dimensions, OriginDimensions, Point, Size},
    pixelcolor::{Rgb565, Rgb888, RgbColor},
    prelude::{IntoStorage, Pixel, Primitive},
    primitives::{PrimitiveStyle, Rectangle},
};
use esp_idf_svc::hal::delay::FreeRtos;
use esp_idf_sys::{
    ESP_ERR_INVALID_STATE, ESP_ERR_NO_MEM, ESP_FAIL, ESP_OK, EspError, MALLOC_CAP_8BIT,
    MALLOC_CAP_DMA, MALLOC_CAP_INTERNAL, MALLOC_CAP_SPIRAM, esp_err_t, esp_lcd_new_panel_io_i2c_v2,
    esp_lcd_panel_draw_bitmap, esp_lcd_panel_io_del, esp_lcd_panel_io_handle_t,
    esp_lcd_panel_io_i2c_config_t, esp_lcd_panel_io_tx_param, esp_lcd_touch_config_t,
    esp_lcd_touch_del, esp_lcd_touch_get_coordinates, esp_lcd_touch_handle_t,
    esp_lcd_touch_new_i2c_gt911, esp_lcd_touch_new_i2c_tt21100, esp_lcd_touch_read_data,
    gpio_num_t_GPIO_NUM_3, gpio_num_t_GPIO_NUM_NC, heap_caps_free, heap_caps_malloc,
};
use log::{debug, error, info};
use rusttype::{Font, Scale, point};

const CANCEL_HEIGHT: u32 = 32;
const CANCEL_WIDTH: u32 = 120;
const CANCEL_X: i32 = (DISPLAY_WIDTH as i32 - CANCEL_WIDTH as i32) / 2;
const CANCEL_Y: i32 = 198 + UI_Y_OFFSET;
const DISPLAY_HEIGHT: usize = 240;
const DISPLAY_WIDTH: usize = 320;
const FONT_SIZE: u32 = 28;
const GT911_ADDRESS: u16 = 0x5d;
const GT911_BACKUP_ADDRESS: u16 = 0x14;
const LOG_TARGET: &str = "WILLOW/UI";
const MAX_SCROLL_BITMAP_WIDTH: usize = 2_048;
const RENDER_STACK_SIZE: usize = 8_192;
const ROW_Y: [i32; 5] = [
    40 + UI_Y_OFFSET,
    70 + UI_Y_OFFSET,
    100 + UI_Y_OFFSET,
    130 + UI_Y_OFFSET,
    160 + UI_Y_OFFSET,
];
const SCROLL_END_DELAY: Duration = Duration::from_millis(300);
const SCROLL_SPEED_PIXELS_PER_SECOND: u128 = 43;
const SCROLL_TEXT_TOP_PADDING: i32 = 3;
const SCROLL_TICK: Duration = Duration::from_millis(60);
const SCROLL_VIEWPORT_HEIGHT: usize = 30;
const SCROLL_VIEWPORT_WIDTH: usize = 300;
const SCROLL_VIEWPORT_X: usize = 10;
const TOUCH_POLL_INTERVAL_MS: u32 = 20;
const TOUCH_STACK_SIZE: usize = 5_120;
const TRANSFER_ROWS: usize = 16;
const TT21100_ADDRESS: u16 = 0x24;
const UI_Y_OFFSET: i32 = 5;

fn willow_background() -> Rgb565 {
    Rgb565::new(0x58 >> 3, 0x37 >> 2, 0x59 >> 3)
}

fn willow_accent() -> Rgb565 {
    Rgb565::new(0xfb >> 3, 0xe8 >> 2, 0x70 >> 3)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LineAlignment {
    Left,
    Center,
}

#[derive(Clone, PartialEq)]
struct Line {
    alignment: LineAlignment,
    color: Rgb565,
    scroll: bool,
    text: Arc<str>,
    visible: bool,
}

impl Default for Line {
    fn default() -> Self {
        Self {
            alignment: LineAlignment::Left,
            color: Rgb565::WHITE,
            scroll: false,
            text: Arc::from(""),
            visible: false,
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum CancelAction {
    #[default]
    None,
    Notification,
    Recording,
}

#[derive(Clone)]
struct UiState {
    cancel_action: CancelAction,
    lines: [Line; 5],
    notification_cancelled: bool,
}

impl UiState {
    fn new() -> Self {
        let mut state = Self {
            cancel_action: CancelAction::None,
            lines: std::array::from_fn(|_| Line::default()),
            notification_cancelled: false,
        };

        state.set_line(
            2,
            "Starting up (server)...",
            LineAlignment::Center,
            Rgb565::WHITE,
        );
        state
    }

    fn hide(&mut self, indices: &[usize]) {
        for &index in indices {
            self.lines[index].visible = false;
        }
    }

    fn set_line(
        &mut self,
        index: usize,
        text: impl AsRef<str>,
        alignment: LineAlignment,
        color: Rgb565,
    ) {
        let line = &mut self.lines[index];
        line.alignment = alignment;
        line.color = color;
        line.scroll = false;
        line.text = Arc::from(text.as_ref());
        line.visible = true;
    }

    fn set_scrolling_line(
        &mut self,
        index: usize,
        text: impl AsRef<str>,
        alignment: LineAlignment,
        color: Rgb565,
    ) {
        self.set_line(index, text, alignment, color);
        self.lines[index].scroll = true;
    }
}

struct UiController {
    redraw: SyncSender<()>,
    state: Arc<Mutex<UiState>>,
    _render_thread: JoinHandle<()>,
}

static UI: OnceLock<UiController> = OnceLock::new();
static TOUCH_THREAD: OnceLock<JoinHandle<()>> = OnceLock::new();

struct HeapBuffer<T> {
    len: usize,
    pointer: NonNull<T>,
}

impl<T> HeapBuffer<T> {
    fn allocate(len: usize, capabilities: u32) -> Result<Self, EspError> {
        let size = len
            .checked_mul(size_of::<T>())
            .ok_or_else(EspError::from_infallible::<ESP_ERR_NO_MEM>)?;
        let pointer = NonNull::new(unsafe { heap_caps_malloc(size, capabilities) }.cast())
            .ok_or_else(EspError::from_infallible::<ESP_ERR_NO_MEM>)?;

        unsafe { ptr::write_bytes(pointer.as_ptr(), 0, len) };
        Ok(Self { len, pointer })
    }

    fn as_ptr(&self) -> *const T {
        self.pointer.as_ptr()
    }

    fn as_slice(&self) -> &[T] {
        unsafe { slice::from_raw_parts(self.pointer.as_ptr(), self.len) }
    }

    fn as_slice_mut(&mut self) -> &mut [T] {
        unsafe { slice::from_raw_parts_mut(self.pointer.as_ptr(), self.len) }
    }
}

impl<T> Drop for HeapBuffer<T> {
    fn drop(&mut self) {
        unsafe { heap_caps_free(self.pointer.as_ptr().cast()) };
    }
}

struct Framebuffer {
    height: usize,
    pixels: HeapBuffer<u16>,
    width: usize,
}

impl Framebuffer {
    fn new() -> Result<Self, EspError> {
        Self::with_size(DISPLAY_WIDTH, DISPLAY_HEIGHT)
    }

    fn with_size(width: usize, height: usize) -> Result<Self, EspError> {
        let pixels = width
            .checked_mul(height)
            .ok_or_else(EspError::from_infallible::<ESP_ERR_NO_MEM>)?;
        Ok(Self {
            height,
            pixels: HeapBuffer::allocate(pixels, MALLOC_CAP_SPIRAM | MALLOC_CAP_8BIT)?,
            width,
        })
    }

    fn fill(&mut self, color: Rgb565) {
        self.pixels
            .as_slice_mut()
            .fill(color.into_storage().swap_bytes());
    }

    fn fill_region(&mut self, x: usize, y: usize, width: usize, height: usize, color: Rgb565) {
        let x_end = (x + width).min(self.width);
        let y_end = (y + height).min(self.height);
        let color = color.into_storage().swap_bytes();
        let pixels = self.pixels.as_slice_mut();
        for row in y..y_end {
            let start = row * self.width + x.min(x_end);
            let end = row * self.width + x_end;
            pixels[start..end].fill(color);
        }
    }

    fn set_pixel(&mut self, x: i32, y: i32, color: u16) {
        if x >= 0 && y >= 0 && x < self.width as i32 && y < self.height as i32 {
            self.pixels.as_slice_mut()[y as usize * self.width + x as usize] = color;
        }
    }

    fn blit(
        &mut self,
        source: &Self,
        source_x: usize,
        destination_x: usize,
        destination_y: usize,
        width: usize,
        height: usize,
    ) {
        let source_pixels = source.pixels.as_slice();
        let destination = self.pixels.as_slice_mut();
        for row in 0..height {
            let source_start = row * source.width + source_x;
            let destination_start = (destination_y + row) * self.width + destination_x;
            destination[destination_start..destination_start + width]
                .copy_from_slice(&source_pixels[source_start..source_start + width]);
        }
    }
}

impl OriginDimensions for Framebuffer {
    fn size(&self) -> Size {
        Size::new(self.width as u32, self.height as u32)
    }
}

impl DrawTarget for Framebuffer {
    type Color = Rgb565;
    type Error = Infallible;

    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Self::Color>>,
    {
        let buffer = self.pixels.as_slice_mut();
        for Pixel(point, color) in pixels {
            if point.x >= 0
                && point.y >= 0
                && point.x < self.width as i32
                && point.y < self.height as i32
            {
                let index = point.y as usize * self.width + point.x as usize;
                buffer[index] = color.into_storage().swap_bytes();
            }
        }
        Ok(())
    }

    fn fill_solid(&mut self, area: &Rectangle, color: Self::Color) -> Result<(), Self::Error> {
        let area = area.intersection(&self.bounding_box());
        let color = color.into_storage().swap_bytes();
        let buffer = self.pixels.as_slice_mut();
        for y in area.rows() {
            let start = y as usize * self.width + area.top_left.x as usize;
            buffer[start..start + area.size.width as usize].fill(color);
        }
        Ok(())
    }
}

struct ScrollAnimation {
    bitmap: Framebuffer,
    max_offset: usize,
    offset: usize,
    started: Instant,
}

impl ScrollAnimation {
    fn offset_at(&self, now: Instant) -> usize {
        let travel_nanoseconds =
            (self.max_offset as u128 * 1_000_000_000).div_ceil(SCROLL_SPEED_PIXELS_PER_SECOND);
        let delay_nanoseconds = SCROLL_END_DELAY.as_nanos();
        let cycle_nanoseconds = 2 * (travel_nanoseconds + delay_nanoseconds);
        let cycle_position = now.duration_since(self.started).as_nanos() % cycle_nanoseconds;

        if cycle_position < travel_nanoseconds {
            pixels_travelled(cycle_position).min(self.max_offset)
        } else if cycle_position < travel_nanoseconds + delay_nanoseconds {
            self.max_offset
        } else if cycle_position < 2 * travel_nanoseconds + delay_nanoseconds {
            self.max_offset
                - pixels_travelled(cycle_position - travel_nanoseconds - delay_nanoseconds)
                    .min(self.max_offset)
        } else {
            0
        }
    }
}

fn pixels_travelled(nanoseconds: u128) -> usize {
    (nanoseconds * SCROLL_SPEED_PIXELS_PER_SECOND / 1_000_000_000) as usize
}

struct BlendTable {
    colors: [u16; 256],
}

impl BlendTable {
    fn new(foreground: Rgb565, background: Rgb565) -> Self {
        let foreground: Rgb888 = foreground.into();
        let background: Rgb888 = background.into();
        Self {
            colors: std::array::from_fn(|alpha| {
                let alpha = alpha as u32;
                let inverse = 255 - alpha;
                let color = Rgb888::new(
                    ((alpha * u32::from(foreground.r()) + inverse * u32::from(background.r()))
                        / 255) as u8,
                    ((alpha * u32::from(foreground.g()) + inverse * u32::from(background.g()))
                        / 255) as u8,
                    ((alpha * u32::from(foreground.b()) + inverse * u32::from(background.b()))
                        / 255) as u8,
                );
                Rgb565::from(color).into_storage().swap_bytes()
            }),
        }
    }

    fn color(&self, coverage: f32) -> u16 {
        self.colors[(coverage.clamp(0.0, 1.0) * 255.0) as usize]
    }
}

fn text_width(font: &Font<'_>, text: &str) -> usize {
    let scale = Scale::uniform(FONT_SIZE as f32);
    let ascent = font.v_metrics(scale).ascent;
    let mut advance = 0;
    let mut pixels = 0;
    for glyph in font.layout(text, scale, point(0.0, ascent)) {
        advance =
            (glyph.position().x + glyph.unpositioned().h_metrics().advance_width).ceil() as i32;
        if let Some(bounds) = glyph.pixel_bounding_box() {
            pixels = pixels.max(bounds.max.x);
        }
    }
    advance.max(pixels).max(0) as usize
}

fn draw_text(
    font: &Font<'_>,
    framebuffer: &mut Framebuffer,
    text: &str,
    anchor: Point,
    alignment: LineAlignment,
    color: Rgb565,
    background: Rgb565,
) {
    let width = text_width(font, text);
    let origin_x = match alignment {
        LineAlignment::Left => anchor.x,
        LineAlignment::Center => anchor.x - width.saturating_sub(1) as i32 / 2,
    };
    let scale = Scale::uniform(FONT_SIZE as f32);
    let ascent = font.v_metrics(scale).ascent;
    let blend = BlendTable::new(color, background);
    for glyph in font.layout(text, scale, point(0.0, ascent)) {
        let Some(bounds) = glyph.pixel_bounding_box() else {
            continue;
        };
        if origin_x + bounds.min.x >= framebuffer.width as i32 {
            break;
        }
        if origin_x + bounds.max.x <= 0 {
            continue;
        }
        glyph.draw(|x, y, coverage| {
            framebuffer.set_pixel(
                origin_x + x as i32 + bounds.min.x,
                anchor.y + y as i32 + bounds.min.y,
                blend.color(coverage),
            );
        });
    }
}

struct Renderer {
    framebuffer: Framebuffer,
    font: Font<'static>,
    io: esp_lcd_panel_io_handle_t,
    panel: esp_idf_sys::esp_lcd_panel_handle_t,
    rendered_state: Option<UiState>,
    scrolls: [Option<ScrollAnimation>; 5],
    transfer: HeapBuffer<u16>,
}

impl Renderer {
    fn new() -> Result<Self, EspError> {
        let (io, panel) = crate::display::handles()
            .ok_or_else(EspError::from_infallible::<ESP_ERR_INVALID_STATE>)?;
        let font =
            Font::try_from_bytes(include_bytes!("../assets/fonts/Tonnelier-Regular.ttf") as &[u8])
                .ok_or_else(|| {
                    error!(target: LOG_TARGET, "failed to parse embedded Tonnelier font");
                    EspError::from_infallible::<ESP_FAIL>()
                })?;
        let mut framebuffer = Framebuffer::new()?;
        framebuffer.fill(willow_background());
        draw_text(
            &font,
            &mut framebuffer,
            "Welcome to Willow!",
            Point::new(DISPLAY_WIDTH as i32 / 2, UI_Y_OFFSET),
            LineAlignment::Center,
            Rgb565::WHITE,
            willow_background(),
        );
        Ok(Self {
            framebuffer,
            font,
            io,
            panel,
            rendered_state: None,
            scrolls: std::array::from_fn(|_| None),
            transfer: HeapBuffer::allocate(
                DISPLAY_WIDTH * TRANSFER_ROWS,
                MALLOC_CAP_DMA | MALLOC_CAP_INTERNAL,
            )?,
        })
    }

    fn render(&mut self, state: &UiState) -> Result<(), EspError> {
        let initial = self.rendered_state.is_none();
        let dirty_lines: [bool; 5] = std::array::from_fn(|index| {
            self.rendered_state
                .as_ref()
                .is_none_or(|rendered| rendered.lines[index] != state.lines[index])
        });
        let cancel_visible = !matches!(state.cancel_action, CancelAction::None);
        let cancel_dirty = self.rendered_state.as_ref().is_none_or(|rendered| {
            let rendered_visible = !matches!(rendered.cancel_action, CancelAction::None);
            rendered_visible != cancel_visible
        });
        let now = Instant::now();

        for (index, &dirty) in dirty_lines.iter().enumerate() {
            if dirty {
                self.render_line(index, &state.lines[index], now)?;
            }
        }
        if cancel_dirty {
            self.render_cancel(cancel_visible);
        }

        if initial {
            self.flush_region(0, 0, DISPLAY_WIDTH, DISPLAY_HEIGHT)?;
        } else {
            for (index, &dirty) in dirty_lines.iter().enumerate() {
                if dirty {
                    self.flush_region(
                        0,
                        (ROW_Y[index] - SCROLL_TEXT_TOP_PADDING) as usize,
                        DISPLAY_WIDTH,
                        SCROLL_VIEWPORT_HEIGHT,
                    )?;
                }
            }
            if cancel_dirty {
                self.flush_region(
                    CANCEL_X as usize,
                    CANCEL_Y as usize,
                    CANCEL_WIDTH as usize,
                    CANCEL_HEIGHT as usize,
                )?;
            }
        }
        self.rendered_state = Some(state.clone());
        Ok(())
    }

    fn render_line(&mut self, index: usize, line: &Line, started: Instant) -> Result<(), EspError> {
        let band_y = (ROW_Y[index] - SCROLL_TEXT_TOP_PADDING) as usize;
        self.framebuffer.fill_region(
            0,
            band_y,
            DISPLAY_WIDTH,
            SCROLL_VIEWPORT_HEIGHT,
            willow_background(),
        );
        self.scrolls[index] = None;
        if !line.visible {
            return Ok(());
        }

        let width = text_width(&self.font, line.text.as_ref());
        if line.scroll && width > SCROLL_VIEWPORT_WIDTH {
            let bitmap_width = width.min(MAX_SCROLL_BITMAP_WIDTH);
            if bitmap_width < width {
                debug!(
                    target: LOG_TARGET,
                    "limiting scrolling line from {width} to {bitmap_width} pixels"
                );
            }
            let mut bitmap = Framebuffer::with_size(bitmap_width, SCROLL_VIEWPORT_HEIGHT)?;
            bitmap.fill(willow_background());
            draw_text(
                &self.font,
                &mut bitmap,
                line.text.as_ref(),
                Point::new(0, SCROLL_TEXT_TOP_PADDING),
                LineAlignment::Left,
                line.color,
                willow_background(),
            );
            self.framebuffer.blit(
                &bitmap,
                0,
                SCROLL_VIEWPORT_X,
                band_y,
                SCROLL_VIEWPORT_WIDTH,
                SCROLL_VIEWPORT_HEIGHT,
            );
            self.scrolls[index] = Some(ScrollAnimation {
                bitmap,
                max_offset: bitmap_width - SCROLL_VIEWPORT_WIDTH,
                offset: 0,
                started,
            });
            return Ok(());
        }

        let x = match line.alignment {
            LineAlignment::Left => 10,
            LineAlignment::Center => DISPLAY_WIDTH as i32 / 2,
        };
        draw_text(
            &self.font,
            &mut self.framebuffer,
            line.text.as_ref(),
            Point::new(x, ROW_Y[index]),
            line.alignment,
            line.color,
            willow_background(),
        );
        Ok(())
    }

    fn render_cancel(&mut self, visible: bool) {
        self.framebuffer.fill_region(
            CANCEL_X as usize,
            CANCEL_Y as usize,
            CANCEL_WIDTH as usize,
            CANCEL_HEIGHT as usize,
            willow_background(),
        );
        if !visible {
            return;
        }
        let button = Rectangle::new(
            Point::new(CANCEL_X, CANCEL_Y),
            Size::new(CANCEL_WIDTH, CANCEL_HEIGHT),
        );
        let _ = button
            .into_styled(PrimitiveStyle::with_fill(willow_accent()))
            .draw(&mut self.framebuffer);
        draw_text(
            &self.font,
            &mut self.framebuffer,
            "Cancel",
            Point::new(DISPLAY_WIDTH as i32 / 2, CANCEL_Y + 2),
            LineAlignment::Center,
            Rgb565::BLACK,
            willow_accent(),
        );
    }

    fn has_scrolling_text(&self) -> bool {
        self.scrolls.iter().any(Option::is_some)
    }

    fn render_scroll_tick(&mut self, now: Instant) -> Result<(), EspError> {
        for (index, &row_y) in ROW_Y.iter().enumerate() {
            let Some(animation) = self.scrolls[index].as_mut() else {
                continue;
            };
            let offset = animation.offset_at(now);
            if offset == animation.offset {
                continue;
            }

            animation.offset = offset;
            self.framebuffer.blit(
                &animation.bitmap,
                offset,
                SCROLL_VIEWPORT_X,
                (row_y - SCROLL_TEXT_TOP_PADDING) as usize,
                SCROLL_VIEWPORT_WIDTH,
                SCROLL_VIEWPORT_HEIGHT,
            );
            self.flush_region(
                SCROLL_VIEWPORT_X,
                (row_y - SCROLL_TEXT_TOP_PADDING) as usize,
                SCROLL_VIEWPORT_WIDTH,
                SCROLL_VIEWPORT_HEIGHT,
            )?;
        }
        Ok(())
    }

    fn flush_region(
        &mut self,
        x: usize,
        y: usize,
        width: usize,
        height: usize,
    ) -> Result<(), EspError> {
        for row_offset in (0..height).step_by(TRANSFER_ROWS) {
            let rows = TRANSFER_ROWS.min(height - row_offset);
            let framebuffer = self.framebuffer.pixels.as_slice();
            let transfer = self.transfer.as_slice_mut();
            for row in 0..rows {
                let source = (y + row_offset + row) * DISPLAY_WIDTH + x;
                let destination = row * width;
                transfer[destination..destination + width]
                    .copy_from_slice(&framebuffer[source..source + width]);
            }
            check(
                unsafe {
                    esp_lcd_panel_draw_bitmap(
                        self.panel,
                        x as i32,
                        (y + row_offset) as i32,
                        (x + width) as i32,
                        (y + row_offset + rows) as i32,
                        self.transfer.as_ptr().cast(),
                    )
                },
                "failed to flush UI stripe",
            )?;
            // A no-command polling transaction drains the queued color DMA
            // before this single transfer buffer is reused.
            check(
                unsafe { esp_lcd_panel_io_tx_param(self.io, -1, ptr::null(), 0) },
                "failed to wait for UI stripe transfer",
            )?;
        }
        Ok(())
    }
}

fn check(result: esp_err_t, operation: &str) -> Result<(), EspError> {
    if let Some(error) = EspError::from(result) {
        error!(target: LOG_TARGET, "{operation}: {error}");
        Err(error)
    } else {
        Ok(())
    }
}

fn render_worker(
    state: Arc<Mutex<UiState>>,
    redraws: Receiver<()>,
    started: SyncSender<Result<(), EspError>>,
) {
    let mut renderer = match Renderer::new() {
        Ok(renderer) => renderer,
        Err(error) => {
            let _ = started.send(Err(error));
            return;
        }
    };
    let _ = started.send(Ok(()));

    loop {
        if renderer.has_scrolling_text() {
            match redraws.recv_timeout(SCROLL_TICK) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout) => {
                    if let Err(error) = renderer.render_scroll_tick(Instant::now()) {
                        error!(target: LOG_TARGET, "failed to scroll UI text: {error}");
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else if redraws.recv().is_err() {
            return;
        }

        let snapshot = state.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Err(error) = renderer.render(&snapshot) {
            error!(target: LOG_TARGET, "failed to render UI: {error}");
        }
    }
}

enum TouchController {
    Gt911,
    Tt21100,
}

struct Touch {
    handle: esp_lcd_touch_handle_t,
    io: esp_lcd_panel_io_handle_t,
}

impl Touch {
    fn new() -> Result<Option<Self>, EspError> {
        if cfg!(esp_idf_esp32_s3_box_lite_board) {
            info!(target: LOG_TARGET, "ESP32-S3-BOX-Lite has no touch controller");
            return Ok(None);
        }

        let (controller, address) = if crate::i2c::probe(GT911_ADDRESS) == ESP_OK {
            (TouchController::Gt911, GT911_ADDRESS)
        } else if crate::i2c::probe(GT911_BACKUP_ADDRESS) == ESP_OK {
            (TouchController::Gt911, GT911_BACKUP_ADDRESS)
        } else if crate::i2c::probe(TT21100_ADDRESS) == ESP_OK {
            (TouchController::Tt21100, TT21100_ADDRESS)
        } else {
            error!(target: LOG_TARGET, "touch screen not detected");
            return Err(EspError::from_infallible::<ESP_FAIL>());
        };

        info!(
            target: LOG_TARGET,
            "detected {} touch controller at 0x{address:02x}",
            match controller {
                TouchController::Gt911 => "GT911",
                TouchController::Tt21100 => "TT21100",
            }
        );

        let mut io_configuration = esp_lcd_panel_io_i2c_config_t {
            dev_addr: u32::from(address),
            control_phase_bytes: 1,
            lcd_cmd_bits: 16,
            scl_speed_hz: 400_000,
            ..Default::default()
        };
        io_configuration.flags.set_disable_control_phase(1);
        let mut io = ptr::null_mut();
        check(
            unsafe {
                esp_lcd_new_panel_io_i2c_v2(
                    crate::i2c::handle()
                        .ok_or_else(EspError::from_infallible::<ESP_ERR_INVALID_STATE>)?,
                    &io_configuration,
                    &mut io,
                )
            },
            "failed to create touch panel IO",
        )?;

        let mut touch_configuration = esp_lcd_touch_config_t {
            x_max: DISPLAY_WIDTH as u16,
            y_max: DISPLAY_HEIGHT as u16,
            rst_gpio_num: gpio_num_t_GPIO_NUM_NC,
            int_gpio_num: gpio_num_t_GPIO_NUM_3,
            ..Default::default()
        };
        if matches!(controller, TouchController::Tt21100) {
            touch_configuration.flags.set_mirror_x(1);
        }

        let mut handle = ptr::null_mut();
        let result = match controller {
            TouchController::Gt911 => unsafe {
                esp_lcd_touch_new_i2c_gt911(io, &touch_configuration, &mut handle)
            },
            TouchController::Tt21100 => unsafe {
                esp_lcd_touch_new_i2c_tt21100(io, &touch_configuration, &mut handle)
            },
        };
        if let Err(error) = check(result, "failed to initialize touch controller") {
            unsafe { esp_lcd_panel_io_del(io) };
            return Err(error);
        }

        Ok(Some(Self { handle, io }))
    }

    fn pressed_at(&self) -> Option<Point> {
        if check(
            unsafe { esp_lcd_touch_read_data(self.handle) },
            "failed to read touch controller",
        )
        .is_err()
        {
            return None;
        }

        let mut x = 0;
        let mut y = 0;
        let mut strength = 0;
        let mut points = 0;
        if unsafe {
            esp_lcd_touch_get_coordinates(
                self.handle,
                &mut x,
                &mut y,
                &mut strength,
                &mut points,
                1,
            )
        } {
            Some(Point::new(i32::from(x), i32::from(y)))
        } else {
            None
        }
    }
}

impl Drop for Touch {
    fn drop(&mut self) {
        unsafe {
            let _ = esp_lcd_touch_del(self.handle);
            let _ = esp_lcd_panel_io_del(self.io);
        }
    }
}

fn point_in_cancel(point: Point) -> bool {
    point.x >= CANCEL_X
        && point.x < CANCEL_X + CANCEL_WIDTH as i32
        && point.y >= CANCEL_Y
        && point.y < CANCEL_Y + CANCEL_HEIGHT as i32
}

fn cancel(state: &Arc<Mutex<UiState>>) {
    let action = state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .cancel_action;

    match action {
        CancelAction::None => {}
        CancelAction::Recording => {
            if let Err(error) = crate::audio::stop_recording() {
                error!(target: LOG_TARGET, "failed to stop recording: {error:#?}");
            }
        }
        CancelAction::Notification => {
            state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .notification_cancelled = true;
            if let Err(error) = crate::audio::cancel_playback() {
                error!(target: LOG_TARGET, "failed to stop notification audio: {error:#?}");
            }
        }
    }
}

fn touch_worker(state: Arc<Mutex<UiState>>, started: SyncSender<Result<(), EspError>>) {
    let touch = match Touch::new() {
        Ok(Some(touch)) => touch,
        Ok(None) => {
            let _ = started.send(Ok(()));
            return;
        }
        Err(error) => {
            let _ = started.send(Err(error));
            return;
        }
    };
    let _ = started.send(Ok(()));
    let mut was_pressed = false;

    loop {
        let point = touch.pressed_at();
        let is_pressed = point.is_some();
        if is_pressed && !was_pressed {
            let _ = crate::backlight::reset_display_timer(true);
            crate::backlight::set(true, false);
            if point.is_some_and(point_in_cancel) {
                debug!(target: LOG_TARGET, "cancel button pressed");
                cancel(&state);
            }
        } else if !is_pressed && was_pressed {
            let _ = crate::backlight::reset_display_timer(false);
        }
        was_pressed = is_pressed;
        FreeRtos::delay_ms(TOUCH_POLL_INTERVAL_MS);
    }
}

/// Initializes UI state and starts the renderer task.
pub(crate) fn initialize() -> Result<(), EspError> {
    if UI.get().is_some() {
        return Err(EspError::from_infallible::<ESP_ERR_INVALID_STATE>());
    }

    let state = Arc::new(Mutex::new(UiState::new()));
    let (redraw, redraws) = sync_channel(1);
    let (render_started, render_startup) = sync_channel(1);
    let render_state = Arc::clone(&state);
    let render_thread = thread::Builder::new()
        .name("ui_render".into())
        .stack_size(RENDER_STACK_SIZE)
        .spawn(move || render_worker(render_state, redraws, render_started))
        .map_err(|error| {
            error!(target: LOG_TARGET, "failed to start UI render task: {error}");
            EspError::from_infallible::<ESP_FAIL>()
        })?;
    render_startup
        .recv()
        .map_err(|_| EspError::from_infallible::<ESP_FAIL>())??;

    UI.set(UiController {
        redraw,
        state,
        _render_thread: render_thread,
    })
    .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())?;
    request_render();
    Ok(())
}

pub(crate) fn initialize_touch() -> Result<(), EspError> {
    if cfg!(esp_idf_esp32_s3_box_lite_board) {
        info!(target: LOG_TARGET, "ESP32-S3-BOX-Lite has no touch controller");
        return Ok(());
    }
    if TOUCH_THREAD.get().is_some() {
        return Ok(());
    }
    let ui = UI
        .get()
        .ok_or_else(EspError::from_infallible::<ESP_ERR_INVALID_STATE>)?;
    let state = Arc::clone(&ui.state);
    let (started, startup) = sync_channel(1);
    let thread = thread::Builder::new()
        .name("ui_touch".into())
        .stack_size(TOUCH_STACK_SIZE)
        .spawn(move || touch_worker(state, started))
        .map_err(|error| {
            error!(target: LOG_TARGET, "failed to start UI touch task: {error}");
            EspError::from_infallible::<ESP_FAIL>()
        })?;
    startup
        .recv()
        .map_err(|_| EspError::from_infallible::<ESP_FAIL>())??;
    TOUCH_THREAD
        .set(thread)
        .map_err(|_| EspError::from_infallible::<ESP_ERR_INVALID_STATE>())
}

fn request_render() {
    let Some(ui) = UI.get() else {
        return;
    };
    match ui.redraw.try_send(()) {
        Ok(()) | Err(TrySendError::Full(())) => {}
        Err(TrySendError::Disconnected(())) => {
            error!(target: LOG_TARGET, "UI render task stopped");
        }
    }
}

fn update(update: impl FnOnce(&mut UiState)) {
    let Some(ui) = UI.get() else {
        error!(target: LOG_TARGET, "UI is not initialized");
        return;
    };
    update(&mut ui.state.lock().unwrap_or_else(PoisonError::into_inner));
    request_render();
}

pub(crate) fn show_connecting(message: &str) {
    update(|state| state.set_line(3, message, LineAlignment::Center, Rgb565::WHITE));
}

pub(crate) fn hide_connecting() {
    update(|state| state.lines[3].visible = false);
}

pub(crate) fn show_center_message(message: &str) {
    update(|state| {
        state.hide(&[0, 1, 3, 4]);
        state.set_line(2, message, LineAlignment::Center, Rgb565::WHITE);
    });
}

pub(crate) fn show_error(primary: &str, secondary: Option<&str>) {
    update(|state| {
        state.hide(&[0, 1, 4]);
        state.lines[3].visible = false;
        state.set_line(2, primary, LineAlignment::Center, Rgb565::WHITE);
        if let Some(secondary) = secondary {
            state.set_line(3, secondary, LineAlignment::Center, Rgb565::WHITE);
        }
    });
}

pub(crate) fn show_command_result(heading: &str, body: &str) {
    update(|state| {
        state.set_line(3, heading, LineAlignment::Left, Rgb565::WHITE);
        state.set_scrolling_line(4, body, LineAlignment::Left, Rgb565::WHITE);
    });
}

pub(crate) fn show_recognition(heading: &str, body: &str) {
    update(|state| {
        state.hide(&[2, 3]);
        state.set_scrolling_line(0, heading, LineAlignment::Left, Rgb565::WHITE);
        state.set_scrolling_line(1, body, LineAlignment::Left, Rgb565::WHITE);
    });
}

pub(crate) fn show_listening() {
    update(|state| {
        state.hide(&[0, 1, 3, 4]);
        state.cancel_action = CancelAction::Recording;
        state.set_line(2, "Say command...", LineAlignment::Center, Rgb565::WHITE);
    });
}

pub(crate) fn show_thinking(multiwake_won: bool) {
    update(|state| {
        state.cancel_action = CancelAction::None;
        state.set_line(
            2,
            if multiwake_won {
                "Thinking..."
            } else {
                "WOW Active - Exiting"
            },
            LineAlignment::Center,
            Rgb565::WHITE,
        );
    });
}

pub(crate) fn show_ready(message: &str) {
    update(|state| {
        state.lines[3].visible = false;
        state.set_line(2, message, LineAlignment::Center, Rgb565::WHITE);
    });
}

pub(crate) fn show_notification(message: Option<&str>) {
    let message = message.unwrap_or("Notification Active");
    update(|state| {
        state.hide(&[0, 1, 3, 4]);
        state.cancel_action = CancelAction::Notification;
        state.notification_cancelled = false;
        state.set_scrolling_line(2, message, LineAlignment::Center, Rgb565::WHITE);
    });
}

pub(crate) fn notification_cancelled() -> bool {
    UI.get().is_some_and(|ui| {
        ui.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .notification_cancelled
    })
}

pub(crate) fn notification_end() {
    update(|state| {
        state.cancel_action = CancelAction::None;
    });
}

unsafe fn text<'a>(pointer: *const c_char) -> Option<Cow<'a, str>> {
    if pointer.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(pointer) }.to_string_lossy())
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_ui_hide_connecting() {
    hide_connecting();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_ui_show_center_message(message: *const c_char) {
    if let Some(message) = unsafe { text(message) } {
        show_center_message(&message);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_ui_show_error(primary: *const c_char, secondary: *const c_char) {
    let Some(primary) = (unsafe { text(primary) }) else {
        return;
    };
    let secondary = unsafe { text(secondary) };
    show_error(&primary, secondary.as_deref());
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_ui_show_command_result(heading: *const c_char, body: *const c_char) {
    let Some(heading) = (unsafe { text(heading) }) else {
        return;
    };
    let Some(body) = (unsafe { text(body) }) else {
        return;
    };
    show_command_result(&heading, &body);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_ui_show_recognition(heading: *const c_char, body: *const c_char) {
    let Some(heading) = (unsafe { text(heading) }) else {
        return;
    };
    let Some(body) = (unsafe { text(body) }) else {
        return;
    };
    show_recognition(&heading, &body);
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_ui_show_listening() {
    show_listening();
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_ui_show_thinking(multiwake_won: bool) {
    show_thinking(multiwake_won);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_ui_show_ready(message: *const c_char) {
    if let Some(message) = unsafe { text(message) } {
        show_ready(&message);
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn rust_ui_show_notification(message: *const c_char) {
    let message = unsafe { text(message) };
    show_notification(message.as_deref());
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_ui_notification_cancelled() -> bool {
    notification_cancelled()
}

#[unsafe(no_mangle)]
pub extern "C" fn rust_ui_notification_end() {
    notification_end();
}
