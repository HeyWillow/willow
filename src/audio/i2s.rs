//! Joint ownership of the full-duplex I2S0 capture and playback channels.

use core::{ffi::c_void, fmt, ptr, ptr::NonNull};

use esp_idf_sys::{
    ESP_ERR_TIMEOUT, EspError, esp_err_t, i2s_chan_config_t, i2s_chan_config_t__bindgen_ty_1,
    i2s_chan_handle_t, i2s_chan_info_t, i2s_channel_disable, i2s_channel_enable,
    i2s_channel_get_info, i2s_channel_init_std_mode, i2s_channel_obj_t, i2s_channel_read,
    i2s_channel_write, i2s_comm_mode_t_I2S_COMM_MODE_STD,
    i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_32BIT, i2s_del_channel, i2s_dir_t_I2S_DIR_RX,
    i2s_dir_t_I2S_DIR_TX, i2s_mclk_multiple_t_I2S_MCLK_MULTIPLE_256, i2s_new_channel,
    i2s_port_t_I2S_NUM_0, i2s_role_t_I2S_ROLE_MASTER,
    i2s_slot_bit_width_t_I2S_SLOT_BIT_WIDTH_32BIT, i2s_slot_mode_t_I2S_SLOT_MODE_STEREO,
    i2s_std_clk_config_t, i2s_std_config_t, i2s_std_gpio_config_t,
    i2s_std_gpio_config_t__bindgen_ty_1, i2s_std_slot_config_t,
    i2s_std_slot_mask_t_I2S_STD_SLOT_BOTH, soc_periph_i2s_clk_src_t_I2S_CLK_SRC_DEFAULT,
};
use log::error;

use super::{board, capture};

// Six descriptors preserve 117 ms of measured-format headroom while capture,
// AFE feed, and AFE fetch run sequentially.
const DMA_DESCRIPTOR_COUNT: u32 = 6;
const DMA_FRAMES_PER_DESCRIPTOR: u32 = 312;
const LOG_TARGET: &str = "WILLOW/AUDIO";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Direction {
    Receive,
    Transmit,
}

impl Direction {
    const fn name(self) -> &'static str {
        match self {
            Self::Receive => "RX",
            Self::Transmit => "TX",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ChannelSnapshot {
    id: u32,
    role: u32,
    direction: u32,
    mode: u32,
    pair: i2s_chan_handle_t,
    dma_buffer_bytes: u32,
}

impl From<i2s_chan_info_t> for ChannelSnapshot {
    fn from(info: i2s_chan_info_t) -> Self {
        Self {
            id: info.id,
            role: info.role,
            direction: info.dir,
            mode: info.mode,
            pair: info.pair_chan,
            dma_buffer_bytes: info.total_dma_buf_size,
        }
    }
}

#[derive(Debug)]
pub(super) enum I2sError {
    Hal {
        operation: &'static str,
        source: EspError,
    },
    MissingAllocatedChannel {
        receive_missing: bool,
        transmit_missing: bool,
    },
    UnexpectedChannelPair {
        receive: ChannelSnapshot,
        transmit: ChannelSnapshot,
    },
    ChannelNotEnabled {
        direction: Direction,
    },
    InvalidTransferCount {
        direction: Direction,
        requested: usize,
        reported: usize,
    },
    UnsupportedHardware,
}

impl fmt::Display for I2sError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hal { operation, source } => write!(formatter, "{operation} failed: {source}"),
            Self::MissingAllocatedChannel {
                receive_missing,
                transmit_missing,
            } => write!(
                formatter,
                "joint I2S0 allocation returned missing handles: RX missing={receive_missing}, TX missing={transmit_missing}"
            ),
            Self::UnexpectedChannelPair { receive, transmit } => write!(
                formatter,
                "I2S0 allocation did not produce the required full-duplex STD pair: RX={receive:?}, TX={transmit:?}"
            ),
            Self::ChannelNotEnabled { direction } => {
                write!(
                    formatter,
                    "I2S0 {} transfer requested before enable",
                    direction.name()
                )
            }
            Self::InvalidTransferCount {
                direction,
                requested,
                reported,
            } => write!(
                formatter,
                "I2S0 {} reported {reported} bytes for a {requested}-byte buffer",
                direction.name()
            ),
            Self::UnsupportedHardware => {
                formatter.write_str("the selected hardware has no I2S0 configuration")
            }
        }
    }
}

impl std::error::Error for I2sError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Hal { source, .. } => Some(source),
            _ => None,
        }
    }
}

struct Channel {
    direction: Direction,
    handle: NonNull<i2s_channel_obj_t>,
    enabled: bool,
}

// SAFETY: ESP-IDF guarantees that every public I2S API is thread-safe. The
// Rust owner also provides exclusive access to state-changing operations and
// transfers or drops each native channel handle exactly once.
unsafe impl Send for Channel {}

impl Channel {
    const fn new(direction: Direction, handle: NonNull<i2s_channel_obj_t>) -> Self {
        Self {
            direction,
            handle,
            enabled: false,
        }
    }

    const fn handle(&self) -> i2s_chan_handle_t {
        self.handle.as_ptr()
    }

    fn initialize(&self, configuration: &i2s_std_config_t) -> Result<(), I2sError> {
        let operation = match self.direction {
            Direction::Receive => "initialize I2S0 RX in standard mode",
            Direction::Transmit => "initialize I2S0 TX in standard mode",
        };
        // SAFETY: this owner holds a live registered channel, and ESP-IDF
        // copies the complete standard-mode configuration synchronously.
        hal_result(operation, unsafe {
            i2s_channel_init_std_mode(self.handle(), configuration)
        })
    }

    fn enable(&mut self) -> Result<(), I2sError> {
        if self.enabled {
            return Ok(());
        }
        let operation = match self.direction {
            Direction::Receive => "enable I2S0 RX",
            Direction::Transmit => "enable I2S0 TX",
        };
        // SAFETY: this owner contains a live READY channel and serializes its
        // state transitions through an exclusive mutable borrow.
        hal_result(operation, unsafe { i2s_channel_enable(self.handle()) })?;
        self.enabled = true;
        Ok(())
    }

    fn disable(&mut self) -> Result<(), I2sError> {
        if !self.enabled {
            return Ok(());
        }
        let operation = match self.direction {
            Direction::Receive => "disable I2S0 RX",
            Direction::Transmit => "disable I2S0 TX",
        };
        // SAFETY: this owner contains a live RUNNING channel and serializes
        // its state transitions through an exclusive mutable borrow.
        hal_result(operation, unsafe { i2s_channel_disable(self.handle()) })?;
        self.enabled = false;
        Ok(())
    }

    fn validate_transfer(&self, requested: usize, reported: usize) -> Result<usize, I2sError> {
        if reported > requested {
            Err(I2sError::InvalidTransferCount {
                direction: self.direction,
                requested,
                reported,
            })
        } else {
            Ok(reported)
        }
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        if let Err(source) = self.disable() {
            error!(
                target: LOG_TARGET,
                "failed to disable I2S0 {} during cleanup: {source}",
                self.direction.name()
            );
        }

        // SAFETY: this owner is the only code which deletes the channel, and
        // it has attempted to return a running channel to READY first.
        if let Some(source) = EspError::from(unsafe { i2s_del_channel(self.handle()) }) {
            error!(
                target: LOG_TARGET,
                "failed to delete I2S0 {} channel: {source}",
                self.direction.name()
            );
        }
    }
}

pub(super) struct ReceiveChannel(Channel);

impl ReceiveChannel {
    pub(super) fn enable(&mut self) -> Result<(), I2sError> {
        self.0.enable()
    }

    pub(super) fn read(
        &mut self,
        destination: &mut [u8],
        timeout_ms: u32,
    ) -> Result<usize, I2sError> {
        if destination.is_empty() {
            return Ok(0);
        }
        if !self.0.enabled {
            return Err(I2sError::ChannelNotEnabled {
                direction: Direction::Receive,
            });
        }

        let mut bytes_read = 0;
        // SAFETY: the destination remains exclusively borrowed for the
        // synchronous call, and this owner contains the live RX handle.
        let result = unsafe {
            i2s_channel_read(
                self.0.handle(),
                destination.as_mut_ptr().cast::<c_void>(),
                destination.len(),
                &raw mut bytes_read,
                timeout_ms,
            )
        };
        if result == ESP_ERR_TIMEOUT && bytes_read > 0 {
            return self.0.validate_transfer(destination.len(), bytes_read);
        }
        hal_result("read I2S0 RX", result)?;
        self.0.validate_transfer(destination.len(), bytes_read)
    }
}

pub(super) struct TransmitChannel(Channel);

impl TransmitChannel {
    pub(super) fn enable(&mut self) -> Result<(), I2sError> {
        self.0.enable()
    }

    pub(super) fn write(&mut self, source: &[u8], timeout_ms: u32) -> Result<usize, I2sError> {
        if source.is_empty() {
            return Ok(0);
        }
        if !self.0.enabled {
            return Err(I2sError::ChannelNotEnabled {
                direction: Direction::Transmit,
            });
        }

        let mut bytes_written = 0;
        // SAFETY: the source remains borrowed for the synchronous call, and
        // this owner contains the live TX handle.
        let result = unsafe {
            i2s_channel_write(
                self.0.handle(),
                source.as_ptr().cast::<c_void>(),
                source.len(),
                &raw mut bytes_written,
                timeout_ms,
            )
        };
        if result == ESP_ERR_TIMEOUT && bytes_written > 0 {
            return self.0.validate_transfer(source.len(), bytes_written);
        }
        hal_result("write I2S0 TX", result)?;
        self.0.validate_transfer(source.len(), bytes_written)
    }
}

pub(super) struct DuplexChannels {
    // RX drops before TX. Both were allocated together, and either deletion
    // leaves the remaining paired channel valid until its own drop runs.
    pub(super) receive: ReceiveChannel,
    pub(super) transmit: TransmitChannel,
}

impl DuplexChannels {
    /// Allocates and verifies one paired full-duplex I2S0 controller.
    pub(super) fn new() -> Result<Self, I2sError> {
        let board = board::selected().ok_or(I2sError::UnsupportedHardware)?;
        let channel_configuration = channel_configuration();
        let standard_configuration = standard_configuration(board.i2s);
        let mut transmit_handle = ptr::null_mut();
        let mut receive_handle = ptr::null_mut();

        // SAFETY: both output locations remain valid for the call. Supplying
        // both is the ESP-IDF operation which establishes paired ownership.
        let allocation_result = unsafe {
            i2s_new_channel(
                &raw const channel_configuration,
                &raw mut transmit_handle,
                &raw mut receive_handle,
            )
        };
        if let Some(source) = EspError::from(allocation_result) {
            delete_unowned_channel(Direction::Receive, receive_handle);
            delete_unowned_channel(Direction::Transmit, transmit_handle);
            return Err(I2sError::Hal {
                operation: "allocate paired I2S0 RX and TX channels",
                source,
            });
        }

        let receive = NonNull::new(receive_handle)
            .map(|handle| ReceiveChannel(Channel::new(Direction::Receive, handle)));
        let transmit = NonNull::new(transmit_handle)
            .map(|handle| TransmitChannel(Channel::new(Direction::Transmit, handle)));
        let (Some(receive), Some(transmit)) = (receive, transmit) else {
            return Err(I2sError::MissingAllocatedChannel {
                receive_missing: receive_handle.is_null(),
                transmit_missing: transmit_handle.is_null(),
            });
        };
        let channels = Self { receive, transmit };

        // ESP-IDF treats TX as the clock-setting side of a full-duplex pair,
        // so initialize it before RX with the exact same framing and pins.
        channels.transmit.0.initialize(&standard_configuration)?;
        channels.receive.0.initialize(&standard_configuration)?;
        channels.verify_pair()?;
        Ok(channels)
    }

    pub(super) const fn dma_buffered_frames() -> u32 {
        DMA_DESCRIPTOR_COUNT * DMA_FRAMES_PER_DESCRIPTOR
    }

    fn verify_pair(&self) -> Result<(), I2sError> {
        let receive = channel_snapshot("query initialized I2S0 RX", self.receive.0.handle())?;
        let transmit = channel_snapshot("query initialized I2S0 TX", self.transmit.0.handle())?;
        let valid = receive.id == i2s_port_t_I2S_NUM_0
            && transmit.id == i2s_port_t_I2S_NUM_0
            && receive.role == i2s_role_t_I2S_ROLE_MASTER
            && transmit.role == i2s_role_t_I2S_ROLE_MASTER
            && receive.direction == i2s_dir_t_I2S_DIR_RX
            && transmit.direction == i2s_dir_t_I2S_DIR_TX
            && receive.mode == i2s_comm_mode_t_I2S_COMM_MODE_STD
            && transmit.mode == i2s_comm_mode_t_I2S_COMM_MODE_STD
            && receive.pair == self.transmit.0.handle()
            && transmit.pair == self.receive.0.handle()
            && receive.dma_buffer_bytes > 0
            && transmit.dma_buffer_bytes > 0;
        if valid {
            Ok(())
        } else {
            Err(I2sError::UnexpectedChannelPair { receive, transmit })
        }
    }
}

const fn channel_configuration() -> i2s_chan_config_t {
    i2s_chan_config_t {
        id: i2s_port_t_I2S_NUM_0,
        role: i2s_role_t_I2S_ROLE_MASTER,
        dma_desc_num: DMA_DESCRIPTOR_COUNT,
        dma_frame_num: DMA_FRAMES_PER_DESCRIPTOR,
        __bindgen_anon_1: i2s_chan_config_t__bindgen_ty_1 { auto_clear: true },
        auto_clear_before_cb: false,
        intr_priority: 0,
    }
}

fn standard_configuration(pins: board::I2sPins) -> i2s_std_config_t {
    i2s_std_config_t {
        clk_cfg: i2s_std_clk_config_t {
            sample_rate_hz: capture::SAMPLE_RATE_HZ,
            clk_src: soc_periph_i2s_clk_src_t_I2S_CLK_SRC_DEFAULT,
            ext_clk_freq_hz: 0,
            mclk_multiple: i2s_mclk_multiple_t_I2S_MCLK_MULTIPLE_256,
        },
        slot_cfg: i2s_std_slot_config_t {
            data_bit_width: i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_32BIT,
            slot_bit_width: i2s_slot_bit_width_t_I2S_SLOT_BIT_WIDTH_32BIT,
            slot_mode: i2s_slot_mode_t_I2S_SLOT_MODE_STEREO,
            slot_mask: i2s_std_slot_mask_t_I2S_STD_SLOT_BOTH,
            ws_width: capture::SLOT_WIDTH_BITS,
            ws_pol: false,
            bit_shift: true,
            left_align: true,
            big_endian: false,
            bit_order_lsb: false,
        },
        gpio_cfg: i2s_std_gpio_config_t {
            mclk: pins.master_clock,
            bclk: pins.bit_clock,
            ws: pins.word_select,
            dout: pins.data_out,
            din: pins.data_in,
            invert_flags: i2s_std_gpio_config_t__bindgen_ty_1::default(),
        },
    }
}

fn delete_unowned_channel(direction: Direction, handle: i2s_chan_handle_t) {
    if handle.is_null() {
        return;
    }
    // SAFETY: a handle returned alongside a failed joint allocation has not
    // escaped and can only still be in the REGISTERED state.
    if let Some(source) = EspError::from(unsafe { i2s_del_channel(handle) }) {
        error!(
            target: LOG_TARGET,
            "failed to delete partially allocated I2S0 {} channel: {source}",
            direction.name()
        );
    }
}

fn channel_snapshot(
    operation: &'static str,
    handle: i2s_chan_handle_t,
) -> Result<ChannelSnapshot, I2sError> {
    let mut info = i2s_chan_info_t::default();
    // SAFETY: the owner keeps the channel live and the output structure is
    // valid for the complete synchronous query.
    hal_result(operation, unsafe {
        i2s_channel_get_info(handle, &raw mut info)
    })?;
    Ok(info.into())
}

fn hal_result(operation: &'static str, result: esp_err_t) -> Result<(), I2sError> {
    EspError::from(result).map_or(Ok(()), |source| Err(I2sError::Hal { operation, source }))
}
