use core::ffi::{CStr, c_void};
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, Ordering};
use std::ffi::CString;
use std::rc::Rc;

use esp_idf_sys::esp_sr as raw;
use sha2::{Digest, Sha256};

use super::fixture::{PACK_HEADER_LENGTH, PackedFile, validate_pack};
use super::{FrameSpec, InputFormat, Sha256Digest, SrError, WakeModel};

const EXPECTED_PARTITION_ADDRESS: u32 = 0x0063_0000;
const EXPECTED_PARTITION_SIZE: u32 = 0x0060_0000;
const MMU_PAGE_SIZE: u64 = 64 * 1024;
const HASH_CHUNK_SIZE: usize = 4096;
const HASH_CHUNK_SIZE_U32: u32 = 4096;

static PROCESS_LEASED: AtomicBool = AtomicBool::new(false);

pub(super) struct Frontend {
    afe: Option<AfeLease>,
    models: Option<ModelLease>,
    frame_spec: FrameSpec,
    model_index: usize,
    _partition_label: CString,
    _wake_model: CString,
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl Frontend {
    pub(super) fn open(model: WakeModel, input: InputFormat) -> Result<Self, SrError> {
        validate_input(input)?;

        let partition_label = CString::new(c"model".to_bytes())
            .map_err(|_| SrError::InternalInvariant("partition label contains NUL"))?;
        let wake_model = CString::new(model.name())
            .map_err(|_| SrError::InternalInvariant("model name contains NUL"))?;

        let partition = Partition::find(partition_label.as_c_str())?;
        partition.validate_geometry()?;
        let expected_model_count = preflight_pack(&partition)?;
        partition.validate_mmap_capacity()?;

        let process_lease = ProcessLease::acquire()?;
        if static_models_are_initialized() {
            return Err(SrError::ExternalModelState);
        }

        let models = ModelLease::load(process_lease, partition_label.as_c_str())?;
        let selected_model = models.require_model(wake_model.as_c_str(), expected_model_count)?;
        let interface = AfeInterface::load()?;
        interface.validate_required_functions()?;

        let mut config = make_config(selected_model.name, input)?;
        let afe = interface.create(&mut config)?;
        let frame_spec = afe.query_frame_spec(input)?;

        Ok(Self {
            afe: Some(afe),
            models: Some(models),
            frame_spec,
            model_index: selected_model.index,
            _partition_label: partition_label,
            _wake_model: wake_model,
            _not_send_or_sync: PhantomData,
        })
    }

    pub(super) const fn frame_spec(&self) -> FrameSpec {
        self.frame_spec
    }

    pub(super) const fn model_index(&self) -> usize {
        self.model_index
    }
}

impl Drop for Frontend {
    fn drop(&mut self) {
        // Proprietary AFE workers must be gone before their mapped model owner.
        drop(self.afe.take());
        drop(self.models.take());
    }
}

struct ProcessLease {
    _not_send_or_sync: PhantomData<Rc<()>>,
}

impl ProcessLease {
    fn acquire() -> Result<Self, SrError> {
        PROCESS_LEASED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| SrError::AlreadyOpen)?;
        Ok(Self {
            _not_send_or_sync: PhantomData,
        })
    }
}

impl Drop for ProcessLease {
    fn drop(&mut self) {
        PROCESS_LEASED.store(false, Ordering::Release);
    }
}

struct Partition {
    ptr: NonNull<esp_idf_sys::esp_partition_t>,
}

impl Partition {
    fn find(label: &CStr) -> Result<Self, SrError> {
        // SAFETY: `label` is NUL-terminated for this call; IDF owns any returned
        // partition descriptor for the process lifetime.
        let ptr = unsafe {
            esp_idf_sys::esp_partition_find_first(
                esp_idf_sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA,
                esp_idf_sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_ANY,
                label.as_ptr(),
            )
        };
        NonNull::new(ptr.cast_mut())
            .map(|ptr| Self { ptr })
            .ok_or(SrError::MissingPartition)
    }

    fn raw(&self) -> &esp_idf_sys::esp_partition_t {
        // SAFETY: `ptr` came from IDF's process-lifetime partition table and this
        // wrapper never mutates or frees it.
        unsafe { self.ptr.as_ref() }
    }

    fn validate_geometry(&self) -> Result<(), SrError> {
        let partition = self.raw();
        if partition.address != EXPECTED_PARTITION_ADDRESS
            || partition.size != EXPECTED_PARTITION_SIZE
            || partition.type_ != esp_idf_sys::esp_partition_type_t_ESP_PARTITION_TYPE_DATA
            || partition.subtype
                != esp_idf_sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_DATA_SPIFFS
            || partition.encrypted
        {
            return Err(SrError::WrongPartitionGeometry {
                address: partition.address,
                size: partition.size,
                partition_type: partition.type_,
                subtype: partition.subtype,
                encrypted: partition.encrypted,
            });
        }
        Ok(())
    }

    fn read(&self, offset: u32, output: &mut [u8]) -> Result<(), SrError> {
        let output_len = u32::try_from(output.len())
            .map_err(|_| SrError::InternalInvariant("partition read is too large"))?;
        let end = offset
            .checked_add(output_len)
            .ok_or(SrError::InternalInvariant("partition read overflows"))?;
        if end > self.raw().size {
            return Err(SrError::PartitionRead {
                offset,
                code: esp_idf_sys::ESP_ERR_INVALID_SIZE,
            });
        }
        let native_offset = usize::try_from(offset)
            .map_err(|_| SrError::InternalInvariant("partition offset does not fit size_t"))?;

        // SAFETY: the descriptor remains live, the range was checked against its
        // size, and `output` is valid writable storage for exactly `len` bytes.
        let code = unsafe {
            esp_idf_sys::esp_partition_read(
                self.ptr.as_ptr(),
                native_offset,
                output.as_mut_ptr().cast::<c_void>(),
                output.len(),
            )
        };
        if code != esp_idf_sys::ESP_OK {
            return Err(SrError::PartitionRead { offset, code });
        }
        Ok(())
    }

    fn validate_mmap_capacity(&self) -> Result<(), SrError> {
        // SAFETY: this is a read-only query of IDF's flash-MMU allocator state.
        let free_pages = unsafe {
            esp_idf_sys::spi_flash_mmap_get_free_pages(
                esp_idf_sys::spi_flash_mmap_memory_t_SPI_FLASH_MMAP_DATA,
            )
        };
        let free_bytes = u64::from(free_pages) * MMU_PAGE_SIZE;
        let required_bytes = u64::from(self.raw().size);
        if free_bytes < required_bytes {
            return Err(SrError::InsufficientMmapSpace {
                free_bytes,
                required_bytes,
            });
        }
        Ok(())
    }
}

fn preflight_pack(partition: &Partition) -> Result<usize, SrError> {
    let mut header = [0_u8; PACK_HEADER_LENGTH];
    partition.read(0, &mut header)?;
    let pack = validate_pack(&header)?;
    let model_count = pack.model_count();
    for packed_file in pack.files() {
        validate_packed_file(partition, packed_file)?;
    }
    Ok(model_count)
}

fn validate_packed_file(partition: &Partition, packed_file: PackedFile) -> Result<(), SrError> {
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; HASH_CHUNK_SIZE];
    let mut offset = packed_file.offset();
    let mut remaining = packed_file.length();

    while remaining != 0 {
        let count = usize::try_from(remaining.min(HASH_CHUNK_SIZE_U32))
            .map_err(|_| SrError::InternalInvariant("hash chunk does not fit usize"))?;
        partition.read(offset, &mut buffer[..count])?;
        hasher.update(&buffer[..count]);
        let count = u32::try_from(count)
            .map_err(|_| SrError::InternalInvariant("hash offset does not fit u32"))?;
        offset = offset
            .checked_add(count)
            .ok_or(SrError::InternalInvariant("hash offset overflows"))?;
        remaining -= count;
    }

    let actual = Sha256Digest(hasher.finalize().into());
    if actual != packed_file.expected_sha256() {
        return Err(SrError::PackedFileHashMismatch {
            model: packed_file.model_name(),
            file: packed_file.file_name(),
            actual,
        });
    }

    Ok(())
}

fn static_models_are_initialized() -> bool {
    // SAFETY: the call only reads ESP-SR's singleton pointer; it does not transfer
    // ownership or dereference the result.
    unsafe { !raw::get_static_srmodels().is_null() }
}

struct ModelLease {
    ptr: NonNull<raw::srmodel_list_t>,
    _process_lease: ProcessLease,
}

struct SelectedModel {
    index: usize,
    name: *mut core::ffi::c_char,
}

impl ModelLease {
    fn load(process_lease: ProcessLease, partition_label: &CStr) -> Result<Self, SrError> {
        // SAFETY: preflight accepted the exact reviewed image, the label remains
        // alive in `Frontend`, and `process_lease` excludes wrapper re-entry.
        let ptr = unsafe { raw::esp_srmodel_init(partition_label.as_ptr()) };
        let ptr = NonNull::new(ptr).ok_or(SrError::ModelLoadFailed)?;
        Ok(Self {
            ptr,
            _process_lease: process_lease,
        })
    }

    fn require_model(
        &self,
        model_name: &CStr,
        expected_model_count: usize,
    ) -> Result<SelectedModel, SrError> {
        let query_name = model_name.as_ptr().cast_mut();
        // SAFETY: both pointers remain valid for the call; ESP-SR does not take
        // ownership of the query string.
        let index = unsafe { raw::esp_srmodel_exists(self.ptr.as_ptr(), query_name) };
        if index < 0 {
            return Err(SrError::MissingWakeModel(
                model_name.to_string_lossy().into_owned(),
            ));
        }
        let index = usize::try_from(index)
            .map_err(|_| SrError::InvalidModelList("model index does not fit usize"))?;

        // SAFETY: `ModelLease` exclusively owns a non-null loader result until
        // `Drop`, and no deinit can run while `self` is borrowed.
        let models = unsafe { self.ptr.as_ref() };
        let model_count = usize::try_from(models.num)
            .map_err(|_| SrError::InvalidModelList("model count does not fit usize"))?;
        if model_count != expected_model_count {
            return Err(SrError::InvalidModelList(
                "model count does not match the reviewed pack",
            ));
        }
        if models.model_name.is_null() {
            return Err(SrError::InvalidModelList("model-name table is null"));
        }
        if index >= model_count {
            return Err(SrError::InvalidModelList(
                "model index falls outside the model-name table",
            ));
        }
        // SAFETY: the loader count matches the preflighted pack, `model_name`
        // is non-null, and `index` was checked against that count.
        let loaded_name = unsafe { *models.model_name.add(index) };
        if loaded_name.is_null() {
            return Err(SrError::InvalidModelList("selected model name is null"));
        }
        // SAFETY: per-file structural preflight proves every packed name is
        // NUL-terminated; ESP-SR copied it into loader-owned live storage.
        let loaded = unsafe { CStr::from_ptr(loaded_name) };
        if loaded != model_name {
            return Err(SrError::InvalidModelList(
                "selected model name differs from the requested model",
            ));
        }

        Ok(SelectedModel {
            index,
            name: loaded_name,
        })
    }
}

impl Drop for ModelLease {
    fn drop(&mut self) {
        // SAFETY: this lease uniquely owns the loader result, and `Frontend::drop`
        // destroys the dependent AFE before dropping this value.
        unsafe { raw::esp_srmodel_deinit(self.ptr.as_ptr()) };
    }
}

#[derive(Clone, Copy)]
struct AfeInterface {
    ptr: NonNull<raw::esp_afe_sr_iface_t>,
}

impl AfeInterface {
    fn load() -> Result<Self, SrError> {
        let ptr = core::ptr::addr_of!(raw::esp_afe_sr_v1).cast_mut();
        NonNull::new(ptr)
            .map(|ptr| Self { ptr })
            .ok_or(SrError::MissingAfeFunction("esp_afe_sr_v1"))
    }

    fn raw(&self) -> &raw::esp_afe_sr_iface_t {
        // SAFETY: `ptr` always addresses the immutable process-lifetime interface.
        unsafe { self.ptr.as_ref() }
    }

    fn validate_required_functions(self) -> Result<(), SrError> {
        let interface = self.raw();
        for (name, present) in [
            ("create_from_config", interface.create_from_config.is_some()),
            ("feed", interface.feed.is_some()),
            ("fetch", interface.fetch.is_some()),
            ("get_feed_chunksize", interface.get_feed_chunksize.is_some()),
            (
                "get_fetch_chunksize",
                interface.get_fetch_chunksize.is_some(),
            ),
            (
                "get_total_channel_num",
                interface.get_total_channel_num.is_some(),
            ),
            ("get_channel_num", interface.get_channel_num.is_some()),
            ("get_samp_rate", interface.get_samp_rate.is_some()),
            ("destroy", interface.destroy.is_some()),
        ] {
            if !present {
                return Err(SrError::MissingAfeFunction(name));
            }
        }
        Ok(())
    }

    fn create(&self, config: &mut raw::afe_config_t) -> Result<AfeLease, SrError> {
        let create = self
            .raw()
            .create_from_config
            .ok_or(SrError::MissingAfeFunction("create_from_config"))?;
        let destroy = self
            .raw()
            .destroy
            .ok_or(SrError::MissingAfeFunction("destroy"))?;
        // SAFETY: the complete config has valid scalar values, and its retained
        // model-name pointer is owned by the longer-lived `ModelLease`.
        let data = unsafe { create(config) };
        let data = NonNull::new(data).ok_or(SrError::AfeCreateFailed)?;
        Ok(AfeLease {
            data,
            interface: *self,
            destroy,
        })
    }
}

struct AfeLease {
    data: NonNull<raw::esp_afe_sr_data_t>,
    interface: AfeInterface,
    destroy: unsafe extern "C" fn(*mut raw::esp_afe_sr_data_t),
}

impl AfeLease {
    fn query_frame_spec(&self, input: InputFormat) -> Result<FrameSpec, SrError> {
        let interface = self.interface.raw();
        let get_sample_rate = interface
            .get_samp_rate
            .ok_or(SrError::MissingAfeFunction("get_samp_rate"))?;
        let get_total_channels = interface
            .get_total_channel_num
            .ok_or(SrError::MissingAfeFunction("get_total_channel_num"))?;
        let get_microphone_channels = interface
            .get_channel_num
            .ok_or(SrError::MissingAfeFunction("get_channel_num"))?;
        let get_feed_chunk = interface
            .get_feed_chunksize
            .ok_or(SrError::MissingAfeFunction("get_feed_chunksize"))?;
        let get_fetch_chunk = interface
            .get_fetch_chunksize
            .ok_or(SrError::MissingAfeFunction("get_fetch_chunksize"))?;

        // SAFETY: all validated function pointers are called with this lease's
        // live AFE handle; the queries do not transfer ownership.
        let dimensions = unsafe {
            (
                get_sample_rate(self.data.as_ptr()),
                get_total_channels(self.data.as_ptr()),
                get_microphone_channels(self.data.as_ptr()),
                get_feed_chunk(self.data.as_ptr()),
                get_fetch_chunk(self.data.as_ptr()),
            )
        };
        let (
            sample_rate,
            input_channels,
            microphone_channels,
            feed_samples_per_channel,
            fetch_samples,
        ) = dimensions;
        let sample_rate = call_dimension("sample rate", sample_rate)?;
        let input_channels = call_dimension("total channel count", input_channels)?;
        let microphone_channels = call_dimension("microphone channel count", microphone_channels)?;
        let feed_samples_per_channel = call_dimension("feed chunk size", feed_samples_per_channel)?;
        let fetch_samples = call_dimension("fetch chunk size", fetch_samples)?;

        let expected_total = input.total_channels()?;
        let expected_sample_rate = usize::try_from(input.sample_rate)
            .map_err(|_| SrError::InvalidAfeDimension("sample rate does not fit usize"))?;
        if sample_rate != expected_sample_rate
            || input_channels != expected_total
            || microphone_channels != input.microphone_channels
        {
            return Err(SrError::UnexpectedAfeDimensions {
                sample_rate,
                input_channels,
                microphone_channels,
            });
        }
        let _ = feed_samples_per_channel
            .checked_mul(input_channels)
            .ok_or(SrError::InvalidAfeDimension("feed frame overflows usize"))?;
        let sample_rate = u32::try_from(sample_rate)
            .map_err(|_| SrError::InvalidAfeDimension("sample rate does not fit u32"))?;

        Ok(FrameSpec {
            sample_rate,
            input_channels,
            microphone_channels,
            reference_channels: input.reference_channels,
            feed_samples_per_channel,
            fetch_samples,
        })
    }
}

impl Drop for AfeLease {
    fn drop(&mut self) {
        // SAFETY: `data` is uniquely owned, still live, and this validated
        // destroy function is invoked exactly once from `Drop`.
        unsafe { (self.destroy)(self.data.as_ptr()) };
    }
}

fn validate_input(input: InputFormat) -> Result<(), SrError> {
    if input.sample_rate != 16_000
        || input.microphone_channels != 2
        || input.reference_channels != 0
    {
        return Err(SrError::UnsupportedInputFormat(input));
    }
    let _ = input.total_channels()?;
    Ok(())
}

fn make_config(
    model_name: *mut core::ffi::c_char,
    input: InputFormat,
) -> Result<raw::afe_config_t, SrError> {
    Ok(raw::afe_config_t {
        aec_init: false,
        se_init: true,
        vad_init: true,
        wakenet_init: true,
        voice_communication_init: false,
        voice_communication_agc_init: false,
        voice_communication_agc_gain: 15,
        vad_mode: raw::vad_mode_t_VAD_MODE_3,
        wakenet_model_name: model_name,
        wakenet_model_name_2: core::ptr::null_mut(),
        wakenet_mode: raw::det_mode_t_DET_MODE_2CH_90,
        afe_mode: raw::afe_sr_mode_t_SR_MODE_HIGH_PERF,
        afe_perferred_core: 1,
        afe_perferred_priority: 5,
        afe_ringbuf_size: 50,
        memory_alloc_mode: raw::afe_memory_alloc_mode_t_AFE_MEMORY_ALLOC_MORE_PSRAM,
        afe_linear_gain: 1.0,
        agc_mode: raw::afe_mn_peak_agc_mode_t_AFE_MN_PEAK_AGC_MODE_3,
        pcm_config: raw::afe_pcm_config_t {
            total_ch_num: i32::try_from(input.total_channels()?)
                .map_err(|_| SrError::InvalidAfeDimension("total channels do not fit i32"))?,
            mic_num: i32::try_from(input.microphone_channels)
                .map_err(|_| SrError::InvalidAfeDimension("mic channels do not fit i32"))?,
            ref_num: i32::try_from(input.reference_channels)
                .map_err(|_| SrError::InvalidAfeDimension("ref channels do not fit i32"))?,
            sample_rate: i32::try_from(input.sample_rate)
                .map_err(|_| SrError::InvalidAfeDimension("sample rate does not fit i32"))?,
        },
        debug_init: false,
        debug_hook: [
            raw::afe_debug_hook_t {
                hook_type: raw::afe_debug_hook_type_t_AFE_DEBUG_HOOK_MASE_TASK_IN,
                hook_callback: None,
            },
            raw::afe_debug_hook_t {
                hook_type: raw::afe_debug_hook_type_t_AFE_DEBUG_HOOK_FETCH_TASK_IN,
                hook_callback: None,
            },
        ],
        afe_ns_mode: raw::afe_ns_mode_t_NS_MODE_SSP,
        afe_ns_model_name: core::ptr::null_mut(),
        fixed_first_channel: true,
    })
}

fn call_dimension(name: &'static str, value: i32) -> Result<usize, SrError> {
    let value = usize::try_from(value).map_err(|_| SrError::InvalidAfeDimension(name))?;
    if value == 0 || value > 65_536 {
        return Err(SrError::InvalidAfeDimension(name));
    }
    Ok(value)
}
