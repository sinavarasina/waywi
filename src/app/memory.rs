use crate::asset::AssetRetention;
use crate::surface::BufferRetention;

/// Runtime memory and cache configuration applied to [`App`](super::App) and
/// [`HeadlessRuntime`](super::HeadlessRuntime).
///
/// Choose a preset ([`MemoryConfig::balanced`], [`MemoryConfig::low_memory`], or
/// [`MemoryConfig::high_throughput`]) and optionally override individual cache limits,
/// retention defaults, or idle teardown behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryConfig {
    /// Default `wl_shm` retention applied to surfaces without an explicit `.buffer_retention(...)`.
    pub default_buffer_retention: BufferRetention,
    /// Default image pixel retention applied when loading disk/bytes image assets.
    pub default_asset_retention: AssetRetention,
    /// Initial capacity reserved per bump slab in `FrameArenaPool`.
    pub arena_initial_bytes: usize,
    /// Maximum bump slab size kept warm across frames before shrinking back to `arena_initial_bytes`.
    pub arena_max_retain_bytes: usize,
    /// Maximum number of shaped text buffers kept in the text shaping cache.
    pub text_max_cached_buffers: usize,
    /// Number of unreferenced frames before a shaped text buffer is evicted.
    pub text_stale_frame_age: u64,
    /// Whether to load system Fontconfig/fontdb fonts at text engine startup.
    pub load_system_fonts: bool,
    /// Maximum reusable software scratch byte capacity retained after a frame completes.
    pub software_scratch_max_bytes: usize,
    /// Maximum cached `Canvas` pixmap byte budget in the software/GPU renderer.
    pub canvas_cache_max_bytes: usize,
    /// Maximum VRAM texture cache budget when `renderer-gpu` is active.
    pub gpu_cache_max_bytes: usize,
    /// Number of unreferenced frames before a cached GPU image texture is evicted.
    pub gpu_stale_frame_age: u64,
    /// Maximum decoded SFX PCM buffer cache budget in [`AudioPlayer`](crate::audio::AudioPlayer).
    pub sound_cache_max_bytes: usize,
    /// Maximum cached entries in [`XdgIconLookup`](crate::desktop::XdgIconLookup).
    pub icon_lookup_max_entries: usize,
    /// Whether `DbusClient` keeps cached copies of subscribed D-Bus service snapshots.
    pub dbus_cache_snapshots: bool,
    /// Whether idle D-Bus shared connections may close when no watchers or lane actions remain.
    pub dbus_idle_disconnect: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self::balanced()
    }
}

impl MemoryConfig {
    /// Balanced default tuning suitable for panels, bars, OSDs, and interactive popups.
    #[must_use]
    pub const fn balanced() -> Self {
        Self {
            default_buffer_retention: BufferRetention::Retain,
            default_asset_retention: AssetRetention::KeepDecoded,
            arena_initial_bytes: 32 * 1024,
            arena_max_retain_bytes: 256 * 1024,
            text_max_cached_buffers: 256,
            text_stale_frame_age: 60,
            load_system_fonts: true,
            software_scratch_max_bytes: 512 * 1024,
            canvas_cache_max_bytes: 8 * 1024 * 1024,
            gpu_cache_max_bytes: 32 * 1024 * 1024,
            gpu_stale_frame_age: 120,
            sound_cache_max_bytes: 8 * 1024 * 1024,
            icon_lookup_max_entries: 2048,
            dbus_cache_snapshots: true,
            dbus_idle_disconnect: true,
        }
    }

    /// Low-memory preset for static wallpapers, minimal bars, and memory-constrained systems.
    ///
    /// Releases idle SHM buffers and decoded images aggressively and disables D-Bus snapshot
    /// caching, at the cost of extra CPU and I/O work when surfaces redraw or resize.
    #[must_use]
    pub const fn low_memory() -> Self {
        Self {
            default_buffer_retention: BufferRetention::ReleaseAfterCommit,
            default_asset_retention: AssetRetention::ReclaimAfterPresent,
            arena_initial_bytes: 16 * 1024,
            arena_max_retain_bytes: 64 * 1024,
            text_max_cached_buffers: 64,
            text_stale_frame_age: 16,
            load_system_fonts: true,
            software_scratch_max_bytes: 64 * 1024,
            canvas_cache_max_bytes: 2 * 1024 * 1024,
            gpu_cache_max_bytes: 8 * 1024 * 1024,
            gpu_stale_frame_age: 30,
            sound_cache_max_bytes: 2 * 1024 * 1024,
            icon_lookup_max_entries: 512,
            dbus_cache_snapshots: false,
            dbus_idle_disconnect: true,
        }
    }

    /// High-throughput preset with larger caches and persistent connections to minimize repeated
    /// allocations, re-shaping, and re-decoding, at the cost of higher baseline RAM and VRAM usage.
    #[must_use]
    pub const fn high_throughput() -> Self {
        Self {
            default_buffer_retention: BufferRetention::Retain,
            default_asset_retention: AssetRetention::KeepDecoded,
            arena_initial_bytes: 64 * 1024,
            arena_max_retain_bytes: 1024 * 1024,
            text_max_cached_buffers: 1024,
            text_stale_frame_age: 240,
            load_system_fonts: true,
            software_scratch_max_bytes: 2 * 1024 * 1024,
            canvas_cache_max_bytes: 32 * 1024 * 1024,
            gpu_cache_max_bytes: 64 * 1024 * 1024,
            gpu_stale_frame_age: 240,
            sound_cache_max_bytes: 32 * 1024 * 1024,
            icon_lookup_max_entries: 8192,
            dbus_cache_snapshots: true,
            dbus_idle_disconnect: false,
        }
    }

    #[must_use]
    pub const fn default_buffer_retention(mut self, retention: BufferRetention) -> Self {
        self.default_buffer_retention = retention;
        self
    }

    #[must_use]
    pub const fn default_asset_retention(mut self, retention: AssetRetention) -> Self {
        self.default_asset_retention = retention;
        self
    }

    #[must_use]
    pub const fn arena_limits(mut self, initial_bytes: usize, max_retain_bytes: usize) -> Self {
        let initial = if initial_bytes == 0 {
            4096
        } else {
            initial_bytes
        };
        let max_retain = if max_retain_bytes < initial {
            initial
        } else {
            max_retain_bytes
        };
        self.arena_initial_bytes = initial;
        self.arena_max_retain_bytes = max_retain;
        self
    }

    #[must_use]
    pub const fn text_cache(mut self, max_buffers: usize, stale_frame_age: u64) -> Self {
        self.text_max_cached_buffers = max_buffers;
        self.text_stale_frame_age = if stale_frame_age == 0 {
            1
        } else {
            stale_frame_age
        };
        self
    }

    #[must_use]
    pub const fn load_system_fonts(mut self, enabled: bool) -> Self {
        self.load_system_fonts = enabled;
        self
    }

    #[must_use]
    pub const fn software_scratch_bytes(mut self, max_bytes: usize) -> Self {
        self.software_scratch_max_bytes = max_bytes;
        self
    }

    #[must_use]
    pub const fn canvas_cache_bytes(mut self, max_bytes: usize) -> Self {
        self.canvas_cache_max_bytes = max_bytes;
        self
    }

    #[must_use]
    pub const fn gpu_cache(mut self, max_bytes: usize, stale_frame_age: u64) -> Self {
        self.gpu_cache_max_bytes = max_bytes;
        self.gpu_stale_frame_age = if stale_frame_age == 0 {
            1
        } else {
            stale_frame_age
        };
        self
    }

    #[must_use]
    pub const fn sound_cache_bytes(mut self, max_bytes: usize) -> Self {
        self.sound_cache_max_bytes = max_bytes;
        self
    }

    #[must_use]
    pub const fn icon_lookup_max_entries(mut self, max_entries: usize) -> Self {
        self.icon_lookup_max_entries = max_entries;
        self
    }

    #[must_use]
    pub const fn dbus_cache_snapshots(mut self, enabled: bool) -> Self {
        self.dbus_cache_snapshots = enabled;
        self
    }

    #[must_use]
    pub const fn dbus_idle_disconnect(mut self, enabled: bool) -> Self {
        self.dbus_idle_disconnect = enabled;
        self
    }
}
