//! Optional on-air DSP via Thimeo libStereoTool (proprietary).
//!
//! The library is NEVER bundled with CrabBoss and never linked: the
//! operator points the config at their own copy (from Thimeo's
//! `Stereo_Tool_Generic_plugin.zip`: `libStereoTool_64.dll` /
//! `libStereoTool_intel64.so` / `.dylib`) plus their license key and a
//! `.sts` preset. It is loaded dynamically at runtime, exactly like
//! LiquidSoap's `stereotool` operator. Without a valid key the library
//! itself overlays voice/beeps, so the license state is surfaced in
//! Settings (see [`StereoTool::license_check`]).
//!
//! Stream-path placement: post-DSP tap → Stereo Tool → encoder. One
//! instance lives on the sender thread (created, used, and deleted
//! there — the handle never crosses threads).
//!
//! FFI declarations below are clean-room signatures read off the SDK
//! 11.05 `libStereoTool.h` (`extern "C"` throughout). C++ `bool` is a
//! 1-byte 0/1 on every supported target, matching Rust `bool`.

use std::ffi::{c_char, CStr, CString};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{CrabError, Result};

// `ID_SAVE_ALLSETTINGS` from the SDK `ParameterEnum.h`: the `enum ID`
// opens at `PARAM_CpuQuality = 0` and contains no conditional
// compilation, so counting entries gives exact values (verified by
// script against the shipped 11.05 header:
// AUDIO=20384, AUDIOFM=20385, ALLSETTINGS=20386, TOTALINI=20387).
const ID_SAVE_ALLSETTINGS: i32 = 20386;

/// Opaque instance handle (`gStereoTool*`).
#[allow(non_camel_case_types)]
enum gStereoTool {}

type StInstance = *mut gStereoTool;

/// Dynamically loaded entry points (subset we use).
#[derive(Debug, Clone, Copy)]
#[allow(non_snake_case)]
struct StApi {
    stereoTool_Create3:
        unsafe extern "C" fn(bool, *const c_char, *const c_char, *const c_char, bool) -> StInstance,
    stereoTool_Delete: unsafe extern "C" fn(StInstance),
    stereoTool_Process: unsafe extern "C" fn(StInstance, *mut f32, i32, i32, i32),
    stereoTool_LoadPreset: unsafe extern "C" fn(StInstance, *const c_char, i32) -> bool,
    stereoTool_CheckLicenseValid: unsafe extern "C" fn(StInstance) -> bool,
    stereoTool_GetUnlicensedUsedFeatures:
        unsafe extern "C" fn(StInstance, *mut c_char, i32) -> bool,
    stereoTool_GetLatency2: unsafe extern "C" fn(StInstance, i32, bool) -> i32,
    stereoTool_GetSoftwareVersion: unsafe extern "C" fn() -> i32,
}

/// A loaded library + one headless instance with a preset.
#[derive(Debug)]
pub struct StereoTool {
    // Held (never touched) so the library outlives the instance and
    // the function pointers below.
    _lib: libloading::Library,
    api: StApi,
    instance: StInstance,
}

// Raw pointers are `Send` (not `Sync`): the instance may move to the
// sender thread, but `&` sharing across threads is a compile error.
unsafe impl Send for StereoTool {}

impl Drop for StereoTool {
    fn drop(&mut self) {
        // Tear down the instance before the library unloads.
        unsafe { (self.api.stereoTool_Delete)(self.instance) };
    }
}

impl StereoTool {
    /// Load the operator's own library, create a headless instance
    /// under `key`, and load `preset_path`. Fails loudly (bad path,
    /// missing symbol, preset rejected) — the caller must not stream
    /// "processed" audio that isn't.
    pub fn open(lib_path: &str, key: &str, preset_path: &str) -> Result<Self> {
        if !Path::new(lib_path).is_file() {
            return Err(CrabError::Audio(format!(
                "Stereo Tool library not found: {lib_path} — point Settings at your own copy from Thimeo's plugin SDK"
            )));
        }
        if !Path::new(preset_path).is_file() {
            return Err(CrabError::Audio(format!(
                "Stereo Tool preset not found: {preset_path}"
            )));
        }
        // SAFETY: a well-formed DLL/SO per Thimeo's SDK.
        let lib = unsafe { libloading::Library::new(lib_path) }
            .map_err(|e| CrabError::Audio(format!("Stereo Tool load {lib_path}: {e}")))?;
        let api = Self::load_api(&lib)?;
        let key_c = CString::new(key)
            .map_err(|e| CrabError::Audio(format!("Stereo Tool key rejected: {e}")))?;
        let key_ptr = if key.is_empty() {
            std::ptr::null()
        } else {
            key_c.as_ptr()
        };
        let name_c = CString::new("CrabBoss").expect("static name");
        // has_gui=false (no GUI code, faster startup), private
        // settings namespace, don't auto-load any .ini.
        let instance = unsafe {
            (api.stereoTool_Create3)(false, key_ptr, name_c.as_ptr(), std::ptr::null(), false)
        };
        if instance.is_null() {
            return Err(CrabError::Audio(
                "Stereo Tool refused to create an instance (license key rejected?)".into(),
            ));
        }
        let preset_c = CString::new(preset_path)
            .map_err(|e| CrabError::Audio(format!("Stereo Tool preset path rejected: {e}")))?;
        let loaded = unsafe {
            (api.stereoTool_LoadPreset)(instance, preset_c.as_ptr(), ID_SAVE_ALLSETTINGS)
        };
        if !loaded {
            unsafe { (api.stereoTool_Delete)(instance) };
            return Err(CrabError::Audio(format!(
                "Stereo Tool rejected preset {preset_path}"
            )));
        }
        Ok(Self {
            _lib: lib,
            api,
            instance,
        })
    }

    /// Load one entry point with its exact type (fails loudly on
    /// version skew instead of calling garbage).
    fn load_api(lib: &libloading::Library) -> Result<StApi> {
        macro_rules! get {
            ($name:ident, $ty:ty) => {
                unsafe {
                    *lib.get::<$ty>(stringify!($name).as_bytes()).map_err(|e| {
                        CrabError::Audio(format!(
                            "Stereo Tool missing symbol {}: {e} (wrong library version?)",
                            stringify!($name),
                        ))
                    })?
                }
            };
        }
        // SAFETY: signatures match SDK 11.05 `libStereoTool.h`.
        Ok(StApi {
            stereoTool_Create3: get!(
                stereoTool_Create3,
                unsafe extern "C" fn(
                    bool,
                    *const c_char,
                    *const c_char,
                    *const c_char,
                    bool,
                ) -> StInstance
            ),
            stereoTool_Delete: get!(stereoTool_Delete, unsafe extern "C" fn(StInstance)),
            stereoTool_Process: get!(
                stereoTool_Process,
                unsafe extern "C" fn(StInstance, *mut f32, i32, i32, i32)
            ),
            stereoTool_LoadPreset: get!(
                stereoTool_LoadPreset,
                unsafe extern "C" fn(StInstance, *const c_char, i32) -> bool
            ),
            stereoTool_CheckLicenseValid: get!(
                stereoTool_CheckLicenseValid,
                unsafe extern "C" fn(StInstance) -> bool
            ),
            stereoTool_GetUnlicensedUsedFeatures: get!(
                stereoTool_GetUnlicensedUsedFeatures,
                unsafe extern "C" fn(StInstance, *mut c_char, i32) -> bool
            ),
            stereoTool_GetLatency2: get!(
                stereoTool_GetLatency2,
                unsafe extern "C" fn(StInstance, i32, bool) -> i32
            ),
            stereoTool_GetSoftwareVersion: get!(
                stereoTool_GetSoftwareVersion,
                unsafe extern "C" fn() -> i32
            ),
        })
    }

    /// Thimeo software version (e.g. 11050 for v11.05).
    pub fn software_version(&self) -> i32 {
        unsafe { (self.api.stereoTool_GetSoftwareVersion)() }
    }

    /// Process interleaved stereo f32 in place (any block length).
    /// Output replaces input 1:1; the first samples are silence while
    /// the internal buffer fills (use [`StereoTool::latency_samples`]
    /// to compensate file-aligned work — irrelevant for live radio).
    pub fn process(&mut self, interleaved: &mut [f32], channels: u32, sample_rate: u32) {
        if interleaved.is_empty() || channels == 0 {
            return;
        }
        let frames = (interleaved.len() / channels as usize) as i32;
        if frames == 0 {
            return;
        }
        // SAFETY: instance live, buffer valid for frames*channels f32.
        unsafe {
            (self.api.stereoTool_Process)(
                self.instance,
                interleaved.as_mut_ptr(),
                frames,
                channels as i32,
                sample_rate as i32,
            )
        };
    }

    /// Processing latency in samples per channel for the active preset
    /// (no silence feed: reliable once audio has flowed — see header).
    pub fn latency_samples(&mut self, sample_rate: u32) -> i32 {
        unsafe { (self.api.stereoTool_GetLatency2)(self.instance, sample_rate as i32, false) }
    }

    /// License health. `CheckLicenseValid` needs audio through the
    /// chain first — call after streaming started; before that it
    /// reports `(false, "verifying…")`.
    pub fn license_check(&mut self, audio_flowed: bool) -> (bool, String) {
        if !audio_flowed {
            return (false, "verifying…".to_string());
        }
        let valid = unsafe { (self.api.stereoTool_CheckLicenseValid)(self.instance) };
        if valid {
            return (true, String::new());
        }
        let mut buf = vec![0 as c_char; 512];
        let ok = unsafe {
            (self.api.stereoTool_GetUnlicensedUsedFeatures)(self.instance, buf.as_mut_ptr(), 512)
        };
        if !ok {
            return (false, "unlicensed features in use".to_string());
        }
        let text = unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        if text.is_empty() {
            (false, "unlicensed features in use".to_string())
        } else {
            (false, text)
        }
    }
}

/// Operator-owned on-air DSP settings (stream path). Everything is
/// inert until `enabled` with a valid library + preset; the license
/// key lives in the local `settings.json` only (gitignored) and is
/// redacted from logs.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StereoToolConfig {
    /// Master switch (takes effect on stream start/restart).
    pub enabled: bool,
    /// Full path to the operator's own libStereoTool copy
    /// (`libStereoTool_64.dll` / `libStereoTool_intel64.so` / `.dylib`).
    pub lib_path: String,
    /// Thimeo license key.
    pub license_key: String,
    /// Full path to the `.sts` preset file.
    pub preset_path: String,
    /// Load everything but pass audio through untouched.
    pub bypass: bool,
}

impl std::fmt::Debug for StereoToolConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StereoToolConfig")
            .field("enabled", &self.enabled)
            .field("lib_path", &self.lib_path)
            .field("license_key", &"<redacted>")
            .field("preset_path", &self.preset_path)
            .field("bypass", &self.bypass)
            .finish()
    }
}

impl StereoToolConfig {
    /// Trim paths; an enabled instance with no library/preset is a
    /// config error the sender reports (never silent-unprocessed).
    pub fn sanitized(mut self) -> Self {
        self.lib_path = self.lib_path.trim().to_string();
        self.preset_path = self.preset_path.trim().to_string();
        self
    }

    /// True when the sender should load the library.
    pub fn wants_processing(&self) -> bool {
        self.enabled && !self.lib_path.is_empty() && !self.preset_path.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bogus_library_path_fails_cleanly() {
        let err = StereoTool::open("definitely/not/here.dll", "", "nope.sts").unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn missing_preset_fails_before_touching_audio() {
        // Point at any existing file as the "library": open must fail
        // on the preset check first (order matters — cheap validation
        // before DLL load).
        let this_file = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let err = StereoTool::open(this_file.to_str().unwrap(), "", "definitely/missing.sts")
            .unwrap_err();
        assert!(err.to_string().contains("preset not found"), "{err}");
    }

    #[test]
    fn config_defaults_are_inert() {
        let cfg = StereoToolConfig::default();
        assert!(!cfg.enabled);
        assert!(!cfg.wants_processing());
        // Old settings.json files without the section stay off.
        let cfg: StereoToolConfig = serde_json::from_str("{}").unwrap();
        assert!(!cfg.enabled);
    }

    #[test]
    fn config_roundtrips_and_sanitizes() {
        let cfg = StereoToolConfig {
            enabled: true,
            lib_path: "  C:/st/lib.dll  ".into(),
            license_key: "k".into(),
            preset_path: " p.sts ".into(),
            bypass: true,
        }
        .sanitized();
        assert_eq!(cfg.lib_path, "C:/st/lib.dll");
        assert_eq!(cfg.preset_path, "p.sts");
        assert!(cfg.wants_processing());
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("C:/st/lib.dll"));
        let back: StereoToolConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn debug_redacts_the_key() {
        let cfg = StereoToolConfig {
            license_key: "super-secret".into(),
            ..Default::default()
        };
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("super-secret"), "key leaked: {dbg}");
        assert!(dbg.contains("<redacted>"));
        // Serialization still carries the real value (local file only).
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("super-secret"));
    }

    /// Live test against the real Thimeo library. Ignored by default:
    /// needs the operator's own copy + key + preset via env, so CI
    /// never touches proprietary bits.
    ///
    /// `CRABBOSS_ST_LIB` = full path to libStereoTool_64.dll/.so
    /// `CRABBOSS_ST_KEY` = license key
    /// `CRABBOSS_ST_PRESET` = full path to a .sts preset
    #[test]
    #[ignore]
    fn live_process_against_real_library() {
        let lib = std::env::var("CRABBOSS_ST_LIB").expect("CRABBOSS_ST_LIB not set");
        let key = std::env::var("CRABBOSS_ST_KEY").unwrap_or_default();
        let preset = std::env::var("CRABBOSS_ST_PRESET").expect("CRABBOSS_ST_PRESET not set");
        let mut st = StereoTool::open(&lib, &key, &preset).expect("open real library");
        let ver = st.software_version();
        assert!(ver > 1000, "suspicious version {ver}");
        // 1 s of stereo sine @48k through the chain: must stay finite.
        let mut buf = vec![0.0f32; 48_000 * 2];
        for (i, s) in buf.iter_mut().enumerate() {
            let t = i as f32 / 2.0 / 48_000.0;
            *s = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.5;
        }
        st.process(&mut buf, 2, 48_000);
        assert!(buf.iter().all(|s| s.is_finite()), "non-finite output");
        assert!(
            buf.iter().any(|s| s.abs() > 1e-6),
            "all-silent output (preset muted?)"
        );
        let (valid, detail) = st.license_check(true);
        eprintln!("Stereo Tool v{ver}, licensed={valid} {detail}");
    }
}
