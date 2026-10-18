//! Tesseract through its C API (`capi.h`), in the process: linked into the static executable,
//! or else the library of the `tesseract` on the `PATH`, with the language data that program
//! reads. An engine per model is loaded once and reused by one reading at a time, as our Java
//! `LinkedTesseract` keeps them, so that reading an image starts no process and loads no model.
//!
//! A reading runs to its end, as the `tesseract` program's does: the C API offers no safe way to
//! judge the words recognized so far while recognition runs, since a result iterator orders
//! right-to-left words by results the words not yet recognized do not have.
//!
//! The tesseract the static executable links, and the bundles' library, are built without
//! OpenMP, as tesseract's own CMake build is by default: readings run side by side on threads of
//! their own, a page each, rather than each dividing its work among a team of tesseract's.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{c_char, c_float, c_int, CStr, CString};
use std::fmt;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long an engine may sit unused before it is freed with the model it loaded.
const IDLE: Duration = Duration::from_secs(5 * 60);
/// The model orientation and script detection runs with.
const OSD_MODEL: &str = "osd";
/// The extension of a model's file.
const MODEL_EXTENSION: &str = ".traineddata";
/// `PSM_OSD_ONLY`.
const OSD_ONLY: c_int = 0;
/// `L_SEVERITY_NONE`: Leptonica reports nothing.
const LEPTONICA_QUIET: c_int = 6;

#[repr(C)]
struct BaseApi {
    _opaque: [u8; 0],
}

#[repr(C)]
struct Monitor {
    _opaque: [u8; 0],
}

/// Leptonica's image.
#[repr(C)]
struct Pix {
    _opaque: [u8; 0],
}

/// Declares the C functions readings call, once: as the static executable links them, and as a
/// table of pointers a loaded library resolves.
macro_rules! c_api {
    ($($field:ident = $symbol:ident($($argument:ty),*) $(-> $result:ty)?;)*) => {
        /// The functions of tesseract's C API, and Leptonica's, that readings call.
        struct Api {
            $($field: unsafe extern "C" fn($($argument),*) $(-> $result)?,)*
        }

        /// The names of the functions readings call, which a bundled library exports.
        #[cfg(test)]
        const SYMBOLS: &[&str] = &[$(stringify!($symbol)),*];

        #[cfg(feature = "static")]
        mod linked {
            use super::*;

            extern "C" {
                $(pub(super) fn $symbol($(_: $argument),*) $(-> $result)?;)*
            }
        }

        impl Api {
            /// The functions linked into the static executable.
            #[cfg(feature = "static")]
            fn linked() -> Self {
                Self { $($field: linked::$symbol,)* }
            }

            /// The functions of a loaded library, or which one it lacks.
            #[cfg(not(feature = "static"))]
            fn resolved(library: &libloading::Library) -> Result<Self, String> {
                Ok(Self {
                    $($field: *unsafe {
                        library.get::<unsafe extern "C" fn($($argument),*) $(-> $result)?>(
                            concat!(stringify!($symbol), "\0").as_bytes(),
                        )
                    }
                    .map_err(|e| format!(
                        "it has no {}: {}",
                        stringify!($symbol),
                        crate::bundle::loader_message(&e)
                    ))?,)*
                })
            }
        }
    };
}

c_api! {
    version = TessVersion() -> *const c_char;
    create = TessBaseAPICreate() -> *mut BaseApi;
    delete = TessBaseAPIDelete(*mut BaseApi);
    init = TessBaseAPIInit3(*mut BaseApi, *const c_char, *const c_char) -> c_int;
    set_variable = TessBaseAPISetVariable(*mut BaseApi, *const c_char, *const c_char) -> c_int;
    set_page_seg_mode = TessBaseAPISetPageSegMode(*mut BaseApi, c_int);
    set_image = TessBaseAPISetImage(*mut BaseApi, *const u8, c_int, c_int, c_int, c_int);
    set_pix = TessBaseAPISetImage2(*mut BaseApi, *mut Pix);
    read_pix = pixReadMem(*const u8, usize) -> *mut Pix;
    destroy_pix = pixDestroy(*mut *mut Pix);
    recognize = TessBaseAPIRecognize(*mut BaseApi, *mut Monitor) -> c_int;
    utf8_text = TessBaseAPIGetUTF8Text(*mut BaseApi) -> *mut c_char;
    delete_text = TessDeleteText(*const c_char);
    mean_text_conf = TessBaseAPIMeanTextConf(*mut BaseApi) -> c_int;
    clear = TessBaseAPIClear(*mut BaseApi);
    detect_orientation_script = TessBaseAPIDetectOrientationScript(
        *mut BaseApi, *mut c_int, *mut c_float, *mut *const c_char, *mut c_float) -> c_int;
    available_languages = TessBaseAPIGetAvailableLanguagesAsVector(*const BaseApi)
        -> *mut *mut c_char;
    delete_text_array = TessDeleteTextArray(*mut *mut c_char);
    create_monitor = TessMonitorCreate() -> *mut Monitor;
    delete_monitor = TessMonitorDelete(*mut Monitor);
    set_deadline = TessMonitorSetDeadlineMSecs(*mut Monitor, c_int);
    set_message_severity = setMsgSeverity(c_int) -> c_int;
}

/// How tesseract segments a page into text, as `--psm` numbers the modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PageSegMode(pub(crate) u8);

/// How tesseract binarises an image, as its `thresholding_method` variable numbers the methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Thresholding {
    /// Otsu's threshold for the whole image.
    Otsu,
    /// Sauvola's adaptive threshold, for images lit unevenly.
    Sauvola,
}

impl Thresholding {
    fn value(self) -> &'static CStr {
        match self {
            Self::Otsu => c"0",
            Self::Sauvola => c"2",
        }
    }
}

/// An image as tesseract reads it.
#[derive(Debug, Clone)]
pub(crate) enum Pixels<'a> {
    /// Grey levels, row by row without padding, a byte each.
    Gray {
        data: &'a [u8],
        width: u32,
        height: u32,
    },
    /// Red, green and blue levels, row by row without padding, three bytes a pixel.
    Rgb {
        data: &'a [u8],
        width: u32,
        height: u32,
    },
    /// An image file, which tesseract decodes with Leptonica, as the `tesseract` program does a
    /// file it is handed: with the resolution the file records, and the transparency of a PNG
    /// laid over white.
    Encoded(Cow<'a, [u8]>),
}

impl Pixels<'_> {
    fn describe(&self) -> String {
        match self {
            Self::Gray { width, height, .. } | Self::Rgb { width, height, .. } => {
                format!("a {width}x{height} image")
            }
            Self::Encoded(data) => format!("an image file of {} bytes", data.len()),
        }
    }

    /// Hands the image to an engine, which copies it: false when Leptonica cannot decode it.
    unsafe fn set_on(&self, api: &Api, handle: *mut BaseApi) -> bool {
        match self {
            Self::Gray {
                data,
                width,
                height,
            } => (api.set_image)(
                handle,
                data.as_ptr(),
                *width as c_int,
                *height as c_int,
                1,
                *width as c_int,
            ),
            Self::Rgb {
                data,
                width,
                height,
            } => (api.set_image)(
                handle,
                data.as_ptr(),
                *width as c_int,
                *height as c_int,
                3,
                (*width * 3) as c_int,
            ),
            Self::Encoded(data) => {
                let mut pix = (api.read_pix)(data.as_ptr(), data.len());
                if pix.is_null() {
                    return false;
                }
                (api.set_pix)(handle, pix);
                (api.destroy_pix)(&mut pix);
            }
        }
        true
    }
}

/// What tesseract read in an image: its text as the text renderer writes it, and the mean
/// confidence of its words from 0 to 100, as `MeanTextConf` takes it, 0 without words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Reading {
    pub(crate) text: String,
    pub(crate) confidence: i32,
}

/// What orientation and script detection found in an image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Detection {
    /// The script, by tesseract's name, or none when detection could not judge the image.
    pub(crate) script: Option<String>,
    /// The clockwise quarter turns that make the image upright.
    pub(crate) clockwise_quarter_turns: u8,
}

/// Why tesseract read nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrError {
    /// It did not finish within the time the reading was given.
    TimedOut { image: String, millis: u128 },
    /// It could not read the image.
    Failed { image: String, model: String },
    /// It could not load a model.
    Model { model: String },
    /// The models it reads with could not be listed.
    Models { dir: String, reason: String },
    /// The tesseract the build carries could not be set up, for this reason.
    Unavailable { reason: String },
}

impl fmt::Display for OcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut { image, millis } => write!(
                f,
                "Reading {image} failed: tesseract did not finish within {millis} ms"
            ),
            Self::Failed { image, model } => write!(
                f,
                "Reading {image} failed: tesseract could not read it with {model}"
            ),
            Self::Model { model } => write!(f, "tesseract cannot load the model {model}"),
            Self::Models { dir, reason } => {
                write!(f, "Listing tesseract's models under {dir} failed: {reason}")
            }
            Self::Unavailable { reason } => write!(f, "tesseract cannot run: {reason}"),
        }
    }
}

/// An engine with a model loaded, used by one reading at a time.
struct Engine(NonNull<BaseApi>);

// SAFETY: an engine is used by one thread at a time, the one the pool hands it to.
unsafe impl Send for Engine {}

/// An engine no reading is using, and since when.
struct Idle {
    engine: Engine,
    since: Instant,
}

/// Tesseract in this process, and its engines.
pub struct Tesseract {
    api: Api,
    /// Where the models are, or none for where tesseract looks by itself.
    datapath: Option<CString>,
    idle: Mutex<BTreeMap<String, Vec<Idle>>>,
    languages: Mutex<Option<BTreeSet<String>>>,
    #[cfg(not(feature = "static"))]
    _library: libloading::Library,
}

impl fmt::Debug for Tesseract {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tesseract")
            .field("datapath", &self.datapath)
            .finish_non_exhaustive()
    }
}

impl Tesseract {
    /// The tesseract linked into the static executable, with the language data under `datapath`.
    #[cfg(feature = "static")]
    pub(crate) fn linked(datapath: &std::path::Path) -> Result<Self, String> {
        Ok(Self::new(Api::linked(), Some(c_path(datapath)?)))
    }

    /// The tesseract of a library, with the language data under `datapath`, or else where it
    /// looks by itself.
    #[cfg(not(feature = "static"))]
    pub(crate) fn loaded(
        library: &std::path::Path,
        datapath: Option<&std::path::Path>,
    ) -> Result<Self, String> {
        let datapath = datapath.map(c_path).transpose()?;
        let loaded = unsafe { libloading::Library::new(library) }.map_err(|e| {
            format!(
                "Loading tesseract's library {} failed: {}",
                library.display(),
                crate::bundle::loader_message(&e)
            )
        })?;
        let api = Api::resolved(&loaded)
            .map_err(|e| format!("tesseract's library {} is unusable: {e}", library.display()))?;
        Ok(Self::new(api, datapath, loaded))
    }

    fn new(
        api: Api,
        datapath: Option<CString>,
        #[cfg(not(feature = "static"))] library: libloading::Library,
    ) -> Self {
        unsafe { (api.set_message_severity)(LEPTONICA_QUIET) };
        Self {
            api,
            datapath,
            idle: Mutex::new(BTreeMap::new()),
            languages: Mutex::new(None),
            #[cfg(not(feature = "static"))]
            _library: library,
        }
    }

    /// The version of the tesseract, as `TessVersion` reports it.
    pub(crate) fn version(&self) -> String {
        unsafe { CStr::from_ptr((self.api.version)()) }
            .to_string_lossy()
            .into_owned()
    }

    /// Reads the text in an image with a model, a page segmentation mode and a thresholding,
    /// as `tesseract <image> stdout --psm <mode> -l <model> -c thresholding_method=<method>`
    /// prints it, with its words' mean confidence, within `timeout` when it has one.
    pub(crate) fn read(
        &self,
        image: Pixels<'_>,
        model: &str,
        segmentation: PageSegMode,
        thresholding: Thresholding,
        timeout: Option<Duration>,
    ) -> Result<Reading, OcrError> {
        let engine = self.take(model)?;
        let result = self.recognize(&engine, image, model, segmentation, thresholding, timeout);
        self.release(model, engine);
        result
    }

    fn recognize(
        &self,
        engine: &Engine,
        image: Pixels<'_>,
        model: &str,
        segmentation: PageSegMode,
        thresholding: Thresholding,
        timeout: Option<Duration>,
    ) -> Result<Reading, OcrError> {
        let api = &self.api;
        let handle = engine.0.as_ptr();
        let failed = || OcrError::Failed {
            image: image.describe(),
            model: model.to_owned(),
        };
        let monitor = MonitorGuard::new(api);
        unsafe {
            (api.set_variable)(
                handle,
                c"thresholding_method".as_ptr(),
                thresholding.value().as_ptr(),
            );
            (api.set_page_seg_mode)(handle, c_int::from(segmentation.0));
            if !image.set_on(api, handle) {
                return Err(failed());
            }
            if let Some(timeout) = timeout {
                let millis = c_int::try_from(timeout.as_millis()).unwrap_or(c_int::MAX);
                (api.set_deadline)(monitor.0, millis);
            }
        }
        let start = Instant::now();
        let status = unsafe { (api.recognize)(handle, monitor.0) };
        let reading = if status < 0 {
            match timeout.filter(|&timeout| start.elapsed() >= timeout) {
                Some(timeout) => Err(OcrError::TimedOut {
                    image: image.describe(),
                    millis: timeout.as_millis(),
                }),
                None => Err(failed()),
            }
        } else {
            unsafe { text_of(api, handle) }.ok_or_else(failed)
        };
        unsafe { (api.clear)(handle) };
        reading
    }

    /// Detects the orientation and script of an image, as `tesseract <image> stdout --psm 0 -c
    /// min_characters_to_try=10` reports them, stopping once it is sure of the first ten
    /// characters it classifies rather than of fifty. An image it cannot judge, such as one with
    /// too little text or one where no script is likelier than another, gives no script and no
    /// turns.
    pub(crate) fn detect(&self, image: Pixels<'_>) -> Result<Detection, OcrError> {
        let engine = self.take(OSD_MODEL)?;
        let api = &self.api;
        let handle = engine.0.as_ptr();
        let mut degrees: c_int = 0;
        let mut orientation_confidence: c_float = 0.0;
        let mut script: *const c_char = std::ptr::null();
        let mut script_confidence: c_float = 0.0;
        let found = unsafe {
            (api.set_page_seg_mode)(handle, OSD_ONLY);
            if !image.set_on(api, handle) {
                self.release(OSD_MODEL, engine);
                return Err(OcrError::Failed {
                    image: image.describe(),
                    model: OSD_MODEL.to_owned(),
                });
            }
            let detected = (api.detect_orientation_script)(
                handle,
                &mut degrees,
                &mut orientation_confidence,
                &mut script,
                &mut script_confidence,
            ) != 0;
            let found = (detected && !script.is_null())
                .then(|| CStr::from_ptr(script).to_string_lossy().into_owned());
            (api.clear)(handle);
            found
        };
        self.release(OSD_MODEL, engine);
        Ok(match found {
            Some(script) if decided(script_confidence) => Detection {
                script: Some(script),
                clockwise_quarter_turns: clockwise_quarter_turns(degrees),
            },
            _ => Detection {
                script: None,
                clockwise_quarter_turns: 0,
            },
        })
    }

    /// The models it reads with, named as `-l` names them, with a slash before a model in a
    /// subdirectory on every platform: `eng`, `script/Latin`.
    pub(crate) fn languages(&self) -> Result<BTreeSet<String>, OcrError> {
        let mut known = self.languages.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(languages) = known.as_ref() {
            return Ok(languages.clone());
        }
        let languages = match self.data_dir() {
            Some(dir) => models_under(&dir)?,
            None => self.listed_languages()?,
        };
        *known = Some(languages.clone());
        Ok(languages)
    }

    /// The directory tesseract reads its models from, when it is known without loading one: the
    /// one it was given, or the one `TESSDATA_PREFIX` names.
    fn data_dir(&self) -> Option<std::path::PathBuf> {
        match &self.datapath {
            Some(datapath) => Some(std::path::PathBuf::from(datapath.to_str().ok()?)),
            None => std::env::var_os("TESSDATA_PREFIX").map(std::path::PathBuf::from),
        }
    }

    /// The models an engine finds where tesseract looks by itself, as `--list-langs` lists them.
    fn listed_languages(&self) -> Result<BTreeSet<String>, OcrError> {
        let engine = self.take(OSD_MODEL)?;
        let languages = unsafe {
            let listed = (self.api.available_languages)(engine.0.as_ptr());
            let mut languages = BTreeSet::new();
            let mut cursor = listed;
            while !cursor.is_null() && !(*cursor).is_null() {
                languages.insert(CStr::from_ptr(*cursor).to_string_lossy().replace('\\', "/"));
                cursor = cursor.add(1);
            }
            if !listed.is_null() {
                (self.api.delete_text_array)(listed);
            }
            languages
        };
        self.release(OSD_MODEL, engine);
        Ok(languages)
    }

    /// An idle engine for a model, the one used last, loaded when every engine for it is in use.
    /// Engines idle for longer than [`IDLE`] are freed first, so that a long-running process
    /// does not keep a model it read with once.
    fn take(&self, model: &str) -> Result<Engine, OcrError> {
        let now = Instant::now();
        let reused = {
            let mut idle = self.idle.lock().unwrap_or_else(|e| e.into_inner());
            for engines in idle.values_mut() {
                engines.retain(|idle| {
                    let keep = now.duration_since(idle.since) < IDLE;
                    if !keep {
                        unsafe { (self.api.delete)(idle.engine.0.as_ptr()) };
                    }
                    keep
                });
            }
            idle.get_mut(model).and_then(Vec::pop)
        };
        match reused {
            Some(idle) => Ok(idle.engine),
            None => self.create(model),
        }
    }

    /// A new engine for a model, set up as readings with it need it.
    fn create(&self, model: &str) -> Result<Engine, OcrError> {
        let unloadable = || OcrError::Model {
            model: model.to_owned(),
        };
        let language = CString::new(model).map_err(|_| unloadable())?;
        let api = &self.api;
        let handle = NonNull::new(unsafe { (api.create)() }).ok_or_else(unloadable)?;
        let datapath = self
            .datapath
            .as_ref()
            .map_or(std::ptr::null(), |path| path.as_ptr());
        let initialized = unsafe {
            (api.set_variable)(
                handle.as_ptr(),
                c"debug_file".as_ptr(),
                c"/dev/null".as_ptr(),
            );
            (api.init)(handle.as_ptr(), datapath, language.as_ptr()) == 0
        };
        if !initialized {
            unsafe { (api.delete)(handle.as_ptr()) };
            return Err(unloadable());
        }
        let variables: &[(&CStr, &CStr)] = if model == OSD_MODEL {
            &[(c"min_characters_to_try", c"10")]
        } else {
            &[
                (c"page_separator", c""),
                (c"preserve_interword_spaces", c"0"),
            ]
        };
        for (name, value) in variables {
            unsafe { (api.set_variable)(handle.as_ptr(), name.as_ptr(), value.as_ptr()) };
        }
        Ok(Engine(handle))
    }

    fn release(&self, model: &str, engine: Engine) {
        self.idle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(model.to_owned())
            .or_default()
            .push(Idle {
                engine,
                since: Instant::now(),
            });
    }
}

impl Drop for Tesseract {
    fn drop(&mut self) {
        let idle = std::mem::take(self.idle.get_mut().unwrap_or_else(|e| e.into_inner()));
        for idle in idle.into_values().flatten() {
            unsafe { (self.api.delete)(idle.engine.0.as_ptr()) };
        }
    }
}

/// A path as tesseract's C API takes it.
fn c_path(path: &std::path::Path) -> Result<CString, String> {
    path.to_str()
        .and_then(|text| CString::new(text).ok())
        .ok_or_else(|| format!("The language data path {} is not UTF-8", path.display()))
}

/// The models under a directory, in its subdirectories too, named as `-l` names them: their
/// paths below it without the `.traineddata`, with a slash between directories on every
/// platform.
fn models_under(dir: &std::path::Path) -> Result<BTreeSet<String>, OcrError> {
    let unlisted = |e: std::io::Error| OcrError::Models {
        dir: dir.display().to_string(),
        reason: e.to_string(),
    };
    let mut models = BTreeSet::new();
    let mut pending = vec![(dir.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = pending.pop() {
        for entry in std::fs::read_dir(&directory).map_err(unlisted)? {
            let entry = entry.map_err(unlisted)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().map_err(unlisted)?.is_dir() {
                pending.push((entry.path(), format!("{prefix}{name}/")));
            } else if let Some(model) = name.strip_suffix(MODEL_EXTENSION) {
                models.insert(format!("{prefix}{model}"));
            }
        }
    }
    Ok(models)
}

/// The text an engine recognized, and its words' mean confidence.
unsafe fn text_of(api: &Api, handle: *mut BaseApi) -> Option<Reading> {
    let text = (api.utf8_text)(handle);
    if text.is_null() {
        return None;
    }
    let read = CStr::from_ptr(text).to_string_lossy().into_owned();
    (api.delete_text)(text);
    Some(Reading {
        text: read,
        confidence: (api.mean_text_conf)(handle),
    })
}

/// Whether detection decided on a script: whether its confidence is above zero. Our Java read
/// it as `GetOsdText` prints it, with two decimals, so that a confidence under 0.005 decided
/// none (#117).
fn decided(script_confidence: c_float) -> bool {
    script_confidence > 0.0
}

/// `OrientationIdToValue` of the orientation detection found, in clockwise quarter turns.
fn clockwise_quarter_turns(degrees: c_int) -> u8 {
    match degrees / 90 {
        1 => 3,
        2 => 2,
        3 => 1,
        _ => 0,
    }
}

/// A progress monitor, freed when dropped.
struct MonitorGuard<'a>(*mut Monitor, &'a Api);

impl<'a> MonitorGuard<'a> {
    fn new(api: &'a Api) -> Self {
        Self(unsafe { (api.create_monitor)() }, api)
    }
}

impl Drop for MonitorGuard<'_> {
    fn drop(&mut self) {
        unsafe { (self.1.delete_monitor)(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ci/tesseract-library` builds the bundled library to export the functions this module calls,
    /// which it lists in `c-api.txt`.
    #[test]
    fn the_bundled_library_exports_every_function_readings_call() {
        let exported = std::fs::read_to_string("../ci/tesseract-library/c-api.txt").unwrap();
        assert_eq!(exported.lines().collect::<Vec<_>>(), SYMBOLS);
    }

    #[test]
    fn turns_as_tesseract_reports_rotation() {
        assert_eq!(clockwise_quarter_turns(0), 0);
        assert_eq!(clockwise_quarter_turns(90), 3);
        assert_eq!(clockwise_quarter_turns(180), 2);
        assert_eq!(clockwise_quarter_turns(270), 1);
    }

    #[test]
    fn lists_models_as_tesseract_names_them() {
        let scratch = crate::formats::parity::Scratch::new("tessdata");
        scratch.write("eng.traineddata", b"");
        scratch.write("notes.txt", b"");
        std::fs::create_dir(scratch.directory().join("script")).unwrap();
        std::fs::write(scratch.directory().join("script/Latin.traineddata"), b"").unwrap();
        let models: Vec<String> = models_under(scratch.directory())
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(models, ["eng", "script/Latin"]);
    }

    #[test]
    fn lists_the_models_tesseract_lists() {
        let tesseract = crate::ocr::tesseract().unwrap();
        assert_eq!(
            tesseract.languages().unwrap(),
            tesseract.listed_languages().unwrap()
        );
    }

    /// Readings and detections on many threads at once, each with an engine of its own taken
    /// from and given back to the shared pool, read what one reading alone reads (#62).
    #[test]
    fn reads_alike_on_many_threads_at_once() {
        let tesseract = crate::ocr::tesseract().unwrap();
        let image =
            std::fs::read("../test_files/documents/table-multi-row-column-cells.png").unwrap();
        let read = || {
            tesseract
                .read(
                    Pixels::Encoded(Cow::Borrowed(&image)),
                    "eng",
                    PageSegMode(3),
                    Thresholding::Otsu,
                    None,
                )
                .unwrap()
        };
        let detect = || {
            tesseract
                .detect(Pixels::Encoded(Cow::Borrowed(&image)))
                .unwrap()
        };
        let (alone, detected) = (read(), detect());
        assert!(alone.text.contains("Participants"), "{}", alone.text);
        std::thread::scope(|scope| {
            let threads: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| (0..3).map(|_| (read(), detect())).collect::<Vec<_>>()))
                .collect();
            for thread in threads {
                for (reading, detection) in thread.join().unwrap() {
                    assert_eq!(reading, alone);
                    assert_eq!(detection, detected);
                }
            }
        });
    }

    #[test]
    fn decides_on_any_confidence_above_zero() {
        assert!(decided(0.01));
        assert!(decided(0.006));
        assert!(decided(0.004));
        assert!(!decided(0.0));
        assert!(!decided(f32::NAN));
    }
}
