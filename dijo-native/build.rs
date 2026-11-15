use std::collections::{HashMap, HashSet};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::hash::{DefaultHasher, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const COMPRESSED_EXTENSION: &str = "gz";
const OCR_BUNDLE_VARIABLE: &str = "DIJO_OCR_BUNDLE";
const OCR_TESSERACT_LIBRARY_STEM: &str = "dijo_tesseract";
const OCR_HEIF_DECODER_STEM: &str = "heif-png";
const OCR_MODELS: &str = "tessdata";
const OCR_MODEL_EXTENSION: &str = "traineddata";
const PPOCR: &str = "ppocr";
const PPOCR_MODEL_EXTENSION: &str = "onnx";
const PPOCR_CONFIGURATION_EXTENSION: &str = "yml";
/// The name a PP-OCR model's graph gives the external data its weights are, which rten requires
/// to be a bare file name ending in `.data`.
const PPOCR_WEIGHTS: &str = "weights.data";
/// The alignment of each tensor of a PP-OCR model's embedded weights, and of the weights
/// themselves: a cache line, beyond what any element type rten reads needs.
const TENSOR_ALIGNMENT: usize = 64;
const OCR_LICENSES: &str = "licenses";
const STATIC_LICENSES_VARIABLE: &str = "DIJO_STATIC_LICENSES";
const STATIC_LIBS_VARIABLE: &str = "DIJO_STATIC_LIBS";
/// The C++ runtime tesseract and libheif need, which `pkg-config` leaves out, and GCC's runtime,
/// whose CPU detection (`__builtin_cpu_supports`) tesseract picks its kernels with.
const STATIC_RUNTIME_LIBRARIES: [&str; 2] = ["stdc++", "gcc"];
/// The libraries of the C library itself, which musl's `libc.a` holds.
const STATIC_C_LIBRARIES: [&str; 4] = ["c", "m", "pthread", "dl"];
fn main() {
    let out_dir = PathBuf::from(required_env("OUT_DIR"));
    let platform = Platform::from_target_os(&required_env("CARGO_CFG_TARGET_OS"));
    if env::var_os("CARGO_FEATURE_STATIC").is_some() {
        let ocr = ocr_bundle(&out_dir, platform, Tesseract::Linked);
        let linked_licenses = static_licenses(&out_dir);
        link_static_libraries();
        write_static_native_module(&out_dir, &ocr, &linked_licenses);
    } else {
        let ocr = ocr_bundle(&out_dir, platform, platform.bundled_tesseract());
        write_ocr_module(&out_dir, &ocr);
    }
}

/// Has Cargo link the static libraries `DIJO_STATIC_LIBS` lists into the program: the files
/// it names, separated as `PATH` is, hold one linker flag per line, `-L<dir>`, `-l<library>` or
/// `-pthread`, as `pkg-config --static --libs` prints them. A library named more than once is
/// linked where it is last named, after everything that needs it, and those of the C library
/// itself (`STATIC_C_LIBRARIES`), which musl's holds whole, are left to it.
fn link_static_libraries() {
    println!("cargo:rerun-if-env-changed={STATIC_LIBS_VARIABLE}");
    let files = env::var_os(STATIC_LIBS_VARIABLE).unwrap_or_else(|| {
        panic!("The static build needs {STATIC_LIBS_VARIABLE} for the libraries it links")
    });
    let mut libraries: Vec<String> = Vec::new();
    for file in env::split_paths(&files) {
        println!("cargo:rerun-if-changed={}", file.display());
        let flags = String::from_utf8(read_file(&file))
            .unwrap_or_else(|e| panic!("{} is not UTF-8: {e}", file.display()));
        for flag in flags.lines() {
            if let Some(dir) = flag.strip_prefix("-L") {
                println!("cargo:rustc-link-search=native={dir}");
            } else if let Some(library) = flag.strip_prefix("-l") {
                libraries.push(library.to_owned());
            } else if flag != "-pthread" {
                panic!(
                    "{} holds {flag:?}, which is no library to link",
                    file.display()
                );
            }
        }
    }
    libraries.extend(STATIC_RUNTIME_LIBRARIES.map(str::to_owned));
    for (index, library) in libraries.iter().enumerate() {
        if STATIC_C_LIBRARIES.contains(&library.as_str())
            || libraries[index + 1..].contains(library)
        {
            continue;
        }
        println!("cargo:rustc-link-lib=static:-bundle={library}");
    }
}

/// The OCR bundle `ci/ocr-bundle.sh` assembles, which `DIJO_OCR_BUNDLE` names: tesseract as
/// a library of the C API native readings call unless the static executable links it, heif-png
/// on the platforms that need it and the language data, each compressed to be unpacked; the
/// PP-OCR models pages tesseract reads poorly are read again with, embedded as they are to be
/// used where they lie; and the licenses of what they hold.
struct OcrBundle {
    /// How the built program runs the bundled tesseract.
    tesseract: Tesseract,
    /// Each file by its path below the unpacked libraries, and where its compressed copy is.
    files: Vec<(String, PathBuf)>,
    /// The PP-OCR models and their configurations.
    ppocr: PpOcrBundle,
    /// The embedded heif-png's file name, if one is embedded.
    heif_decoder: Option<String>,
    /// The licenses of the bundle, as one text.
    licenses: PathBuf,
    fingerprint: String,
}

/// How the built program runs tesseract, which decides what it embeds of the OCR bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tesseract {
    /// The bundled library of tesseract's C API of this name, unpacked and loaded into the process.
    Library(String),
    /// tesseract linked into the static executable, which needs only the language data.
    Linked,
}

impl Tesseract {
    /// The bundled library, unless tesseract is linked into the static executable.
    fn library(&self) -> Option<&str> {
        match self {
            Tesseract::Library(file) => Some(file),
            Tesseract::Linked => None,
        }
    }
}

/// Every build reads with the tesseract it carries, never one it finds where it runs, so a build
/// without the bundle fails.
fn ocr_bundle(out_dir: &Path, platform: Platform, tesseract: Tesseract) -> OcrBundle {
    println!("cargo:rerun-if-env-changed={OCR_BUNDLE_VARIABLE}");
    let dir = PathBuf::from(env::var_os(OCR_BUNDLE_VARIABLE).unwrap_or_else(|| {
        panic!(
            "dijo embeds the tesseract it reads with: {OCR_BUNDLE_VARIABLE} must name an OCR \
             bundle, as the development shell sets it or `just ocr-bundle` assembles one"
        )
    }));
    println!("cargo:rerun-if-changed={}", dir.display());
    let models = bundled_models(&dir, OCR_MODELS, OCR_MODEL_EXTENSION);
    let compressed_dir = out_dir.join("ocr");
    remove_dir_if_exists(&compressed_dir);
    create_dir(&compressed_dir.join(OCR_MODELS));
    let ppocr = ppocr_bundle(&dir.join(PPOCR), &out_dir.join(PPOCR));
    let mut hasher = DefaultHasher::new();
    let mut files = Vec::with_capacity(models.len() + 2);
    let heif_decoder = (tesseract != Tesseract::Linked && platform.heif_decoder_bundled())
        .then(|| platform.executable(OCR_HEIF_DECODER_STEM));
    let named = tesseract
        .library()
        .map(str::to_string)
        .into_iter()
        .chain(heif_decoder.clone())
        .map(|name| {
            let path = dir.join(&name);
            (name, path)
        })
        .chain(models);
    for (name, path) in named {
        let content = read_file(&path);
        hasher.write(name.as_bytes());
        hasher.write(&content);
        let compressed = compressed_dir.join(format!("{name}.{COMPRESSED_EXTENSION}"));
        if let Some(parent) = compressed.parent() {
            create_dir(parent);
        }
        compress_bytes(&content, &compressed);
        files.push((name, compressed));
    }
    let licenses = compressed_dir.join("LICENSES.md");
    write_licenses(&dir.join(OCR_LICENSES), OCR_BUNDLE_VARIABLE, &licenses);
    OcrBundle {
        tesseract,
        files,
        ppocr,
        heif_decoder,
        licenses,
        fingerprint: format!("{:016x}", hasher.finish()),
    }
}

/// The PP-OCR models of the bundle, each as its graph and its weights apart, and the
/// configurations their labels are read from.
struct PpOcrBundle {
    /// Each model by its file's stem, with where its graph and its weights were written.
    models: Vec<(String, PathBuf, PathBuf)>,
    /// Each configuration by its file's stem, with where it is.
    configurations: Vec<(String, PathBuf)>,
}

/// Splits each model in `dir` into its graph, which names its weights as the external data
/// `PPOCR_WEIGHTS`, and those weights, each tensor at a multiple of `TENSOR_ALIGNMENT`, both
/// written to `out_dir`, so that rten uses the embedded weights where they lie rather than copying
/// each out of the graph.
fn ppocr_bundle(dir: &Path, out_dir: &Path) -> PpOcrBundle {
    remove_dir_if_exists(out_dir);
    create_dir(out_dir);
    let mut bundle = PpOcrBundle {
        models: Vec::new(),
        configurations: Vec::new(),
    };
    for path in list_dir(dir) {
        let stem = path
            .file_stem()
            .and_then(OsStr::to_str)
            .unwrap_or_else(|| panic!("PP-OCR file {} has no UTF-8 name", path.display()))
            .to_owned();
        match path.extension().and_then(OsStr::to_str) {
            Some(PPOCR_MODEL_EXTENSION) => {
                let (graph, weights) = split_weights(&read_file(&path))
                    .unwrap_or_else(|e| panic!("{} is no ONNX model: {e}", path.display()));
                let graph_path = out_dir.join(format!("{stem}.{PPOCR_MODEL_EXTENSION}"));
                let weights_path = out_dir.join(format!("{stem}.{PPOCR_WEIGHTS}"));
                write_file(&graph_path, &graph);
                write_file(&weights_path, &weights);
                bundle.models.push((stem, graph_path, weights_path));
            }
            Some(PPOCR_CONFIGURATION_EXTENSION) => bundle.configurations.push((stem, path)),
            _ => panic!(
                "{OCR_BUNDLE_VARIABLE} holds {}, which is neither a PP-OCR model (.{PPOCR_MODEL_EXTENSION}) nor \
                 its configuration (.{PPOCR_CONFIGURATION_EXTENSION})",
                path.display()
            ),
        }
    }
    if bundle.models.is_empty() {
        panic!(
            "{OCR_BUNDLE_VARIABLE} names {}, which holds no {PPOCR}/*.{PPOCR_MODEL_EXTENSION}",
            dir.display()
        );
    }
    bundle
}

/// The fields of the ONNX messages a model's weights are moved out of (onnx/onnx.proto).
const MODEL_GRAPH: u64 = 7;
const GRAPH_INITIALIZER: u64 = 5;
const TENSOR_RAW_DATA: u64 = 9;
const TENSOR_EXTERNAL_DATA: u64 = 13;
const GRAPH_NODE: u64 = 1;
const GRAPH_OUTPUT: u64 = 12;
const GRAPH_VALUE_INFO: u64 = 13;
const NODE_INPUT: u64 = 1;
const NODE_OUTPUT: u64 = 2;
const NODE_OP_TYPE: u64 = 4;
const NODE_ATTRIBUTE: u64 = 5;
const NODE_DOMAIN: u64 = 7;
const ATTRIBUTE_NAME: u64 = 1;
const ATTRIBUTE_FLOAT: u64 = 2;
const VALUE_INFO_NAME: u64 = 1;
const TENSOR_DIMS: u64 = 1;
const TENSOR_DATA_TYPE: u64 = 2;
const TENSOR_NAME: u64 = 8;
const DATA_TYPE_FLOAT: u64 = 1;
/// The `epsilon` of a `BatchNormalization` that sets none (onnx/defs/nn/defs.cc).
const BATCH_NORMALIZATION_EPSILON: f32 = 1e-5;
const TENSOR_DATA_LOCATION: u64 = 14;
const ENTRY_KEY: u64 = 1;
const ENTRY_VALUE: u64 = 2;
const DATA_LOCATION_EXTERNAL: u64 = 1;
const WIRE_VARINT: u64 = 0;
const WIRE_FIXED64: u64 = 1;
const WIRE_LENGTH_DELIMITED: u64 = 2;
const WIRE_FIXED32: u64 = 5;

/// An ONNX model with the `raw_data` of each of its graph's initializers moved into one external
/// data file, each at a multiple of `TENSOR_ALIGNMENT`: the model, referring to `PPOCR_WEIGHTS`
/// at those offsets, and that file.
fn split_weights(model: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let mut weights = Vec::new();
    let mut graph = Vec::with_capacity(model.len() / 8);
    let mut graphs = 0;
    for field in fields(model) {
        let field = field?;
        match (field.number, field.payload) {
            (MODEL_GRAPH, Payload::Bytes(payload)) => {
                graphs += 1;
                let folded = with_batch_norms_folded(payload)?;
                let rewritten = without_weights(&folded, &mut weights)?;
                write_bytes_field(&mut graph, MODEL_GRAPH, &rewritten);
            }
            _ => graph.extend_from_slice(field.encoded),
        }
    }
    if graphs != 1 {
        return Err(format!("it holds {graphs} graphs, not one"));
    }
    Ok((graph, weights))
}

/// A graph with the `raw_data` of each initializer appended to `weights`.
fn without_weights(graph: &[u8], weights: &mut Vec<u8>) -> Result<Vec<u8>, String> {
    let mut rewritten = Vec::with_capacity(graph.len() / 8);
    for field in fields(graph) {
        let field = field?;
        match (field.number, field.payload) {
            (GRAPH_INITIALIZER, Payload::Bytes(tensor)) => {
                let tensor = externalized(tensor, weights)?;
                write_bytes_field(&mut rewritten, GRAPH_INITIALIZER, &tensor);
            }
            _ => rewritten.extend_from_slice(field.encoded),
        }
    }
    Ok(rewritten)
}

/// A tensor whose `raw_data` is appended to `weights`, at the next multiple of
/// `TENSOR_ALIGNMENT`, and referred to there as external data; a tensor without `raw_data` as it
/// is.
fn externalized(tensor: &[u8], weights: &mut Vec<u8>) -> Result<Vec<u8>, String> {
    let mut kept = Vec::with_capacity(tensor.len().min(256));
    let mut raw_data = None;
    for field in fields(tensor) {
        let field = field?;
        match (field.number, field.payload) {
            (TENSOR_RAW_DATA, Payload::Bytes(data)) if raw_data.is_none() => raw_data = Some(data),
            (TENSOR_RAW_DATA, _) => return Err("a tensor holds raw_data twice".to_owned()),
            (TENSOR_EXTERNAL_DATA | TENSOR_DATA_LOCATION, _) => {
                return Err("a tensor already lies in external data".to_owned())
            }
            _ => kept.extend_from_slice(field.encoded),
        }
    }
    let Some(data) = raw_data else {
        return Ok(tensor.to_vec());
    };
    weights.resize(weights.len().next_multiple_of(TENSOR_ALIGNMENT), 0);
    let offset = weights.len();
    weights.extend_from_slice(data);
    for (key, value) in [
        ("location", PPOCR_WEIGHTS.to_owned()),
        ("offset", offset.to_string()),
        ("length", data.len().to_string()),
    ] {
        let mut entry = Vec::with_capacity(key.len() + value.len() + 4);
        write_bytes_field(&mut entry, ENTRY_KEY, key.as_bytes());
        write_bytes_field(&mut entry, ENTRY_VALUE, value.as_bytes());
        write_bytes_field(&mut kept, TENSOR_EXTERNAL_DATA, &entry);
    }
    write_varint(&mut kept, TENSOR_DATA_LOCATION << 3 | WIRE_VARINT);
    write_varint(&mut kept, DATA_LOCATION_EXTERNAL);
    Ok(kept)
}

/// A graph with each `BatchNormalization` of the output of a `Conv`, directly, through
/// `Identity` nodes or through an `Add` of a per-channel bias, folded into that `Conv`'s
/// weights and bias, computed as rten's `ConvAddFusion` and `ConvBatchNormFusion` compute them
/// when they load it: rten then has nothing to fold, so that the weights it uses are the embedded
/// ones where they lie rather than copies of them. A batch normalization folds only when every
/// value between it and the `Conv` and every constant it and the `Conv` read is used by nothing
/// else, so that what the fold removes is unused; any other is left to rten.
fn with_batch_norms_folded(graph: &[u8]) -> Result<Vec<u8>, String> {
    let mut nodes = Vec::new();
    let mut tensors = HashMap::new();
    let mut outputs = Vec::new();
    for field in fields(graph) {
        let field = field?;
        match (field.number, field.payload) {
            (GRAPH_NODE, Payload::Bytes(node)) => nodes.push(Node::of(node)?),
            (GRAPH_INITIALIZER, Payload::Bytes(tensor)) => {
                if let Some(float) = FloatTensor::of(tensor)? {
                    tensors.insert(tensor_name(tensor)?, float);
                }
            }
            (GRAPH_OUTPUT, Payload::Bytes(value)) => outputs.push(value_name(value)?),
            _ => {}
        }
    }
    let topology = Topology::of(&nodes, &tensors, &outputs);
    let folds: Vec<Fold> = (0..nodes.len())
        .filter_map(|index| topology.fold(index))
        .collect();
    if folds.is_empty() {
        return Ok(graph.to_vec());
    }
    let removed_nodes: HashSet<usize> = folds.iter().flat_map(|fold| fold.nodes.clone()).collect();
    let removed_values: HashSet<&str> = folds.iter().flat_map(|fold| fold.values.clone()).collect();
    let removed_tensors: HashSet<&str> =
        folds.iter().flat_map(|fold| fold.tensors.clone()).collect();
    let rewired: HashMap<usize, &Fold> = folds.iter().map(|fold| (fold.conv, fold)).collect();
    let replaced: HashMap<&str, &[f32]> = folds
        .iter()
        .flat_map(|fold| {
            [
                (fold.weight, fold.weights.as_slice()),
                (fold.bias, fold.biases.as_slice()),
            ]
        })
        .collect();
    let mut folded = Vec::with_capacity(graph.len());
    let mut node_index = 0;
    for field in fields(graph) {
        let field = field?;
        match (field.number, field.payload) {
            (GRAPH_NODE, _) => {
                let index = node_index;
                node_index += 1;
                if removed_nodes.contains(&index) {
                    continue;
                }
                match rewired.get(&index) {
                    Some(fold) => write_bytes_field(
                        &mut folded,
                        GRAPH_NODE,
                        &nodes[index]
                            .rewired(&[fold.input, fold.weight, fold.bias], &[fold.output])?,
                    ),
                    None => folded.extend_from_slice(field.encoded),
                }
            }
            (GRAPH_INITIALIZER, Payload::Bytes(tensor)) => {
                let name = tensor_name(tensor)?;
                if removed_tensors.contains(name) {
                    continue;
                }
                match replaced.get(name) {
                    Some(values) => write_bytes_field(
                        &mut folded,
                        GRAPH_INITIALIZER,
                        &with_raw_data(tensor, values)?,
                    ),
                    None => folded.extend_from_slice(field.encoded),
                }
            }
            (GRAPH_VALUE_INFO, Payload::Bytes(value))
                if removed_values.contains(value_name(value)?) => {}
            _ => folded.extend_from_slice(field.encoded),
        }
    }
    Ok(folded)
}

/// A node of an ONNX graph, as folding batch normalizations reads it.
struct Node<'a> {
    encoded: &'a [u8],
    op_type: &'a str,
    domain: &'a str,
    inputs: Vec<&'a str>,
    outputs: Vec<&'a str>,
    epsilon: Option<f32>,
}

impl<'a> Node<'a> {
    fn of(encoded: &'a [u8]) -> Result<Self, String> {
        let mut node = Node {
            encoded,
            op_type: "",
            domain: "",
            inputs: Vec::new(),
            outputs: Vec::new(),
            epsilon: None,
        };
        for field in fields(encoded) {
            let field = field?;
            let Payload::Bytes(bytes) = field.payload else {
                continue;
            };
            match field.number {
                NODE_INPUT => node.inputs.push(utf8(bytes)?),
                NODE_OUTPUT => node.outputs.push(utf8(bytes)?),
                NODE_OP_TYPE => node.op_type = utf8(bytes)?,
                NODE_DOMAIN => node.domain = utf8(bytes)?,
                NODE_ATTRIBUTE => {
                    if let Some(epsilon) = epsilon_of(bytes)? {
                        node.epsilon = Some(epsilon);
                    }
                }
                _ => {}
            }
        }
        Ok(node)
    }

    /// Whether the node is the operator of the default ONNX domain named `op_type`.
    fn is(&self, op_type: &str) -> bool {
        self.op_type == op_type && matches!(self.domain, "" | "ai.onnx")
    }

    /// The node reading `inputs` and writing `outputs` instead of its own.
    fn rewired(&self, inputs: &[&str], outputs: &[&str]) -> Result<Vec<u8>, String> {
        let mut rewired = Vec::with_capacity(self.encoded.len() + 64);
        for input in inputs {
            write_bytes_field(&mut rewired, NODE_INPUT, input.as_bytes());
        }
        for output in outputs {
            write_bytes_field(&mut rewired, NODE_OUTPUT, output.as_bytes());
        }
        for field in fields(self.encoded) {
            let field = field?;
            if !matches!(field.number, NODE_INPUT | NODE_OUTPUT) {
                rewired.extend_from_slice(field.encoded);
            }
        }
        Ok(rewired)
    }
}

/// The value of an `epsilon` attribute of a node, or none for any other attribute.
fn epsilon_of(attribute: &[u8]) -> Result<Option<f32>, String> {
    let mut name = None;
    let mut value = None;
    for field in fields(attribute) {
        let field = field?;
        match (field.number, field.payload) {
            (ATTRIBUTE_NAME, Payload::Bytes(bytes)) => name = Some(bytes),
            (ATTRIBUTE_FLOAT, Payload::Fixed32(bits)) => value = Some(f32::from_bits(bits)),
            _ => {}
        }
    }
    Ok(value.filter(|_| name == Some(b"epsilon".as_slice())))
}

/// An initializer of 32-bit floats held as `raw_data`.
struct FloatTensor {
    dims: Vec<usize>,
    values: Vec<f32>,
}

impl FloatTensor {
    /// The tensor, unless it holds no 32-bit floats or holds them other than as `raw_data`.
    fn of(tensor: &[u8]) -> Result<Option<Self>, String> {
        let mut dims = Vec::new();
        let mut data_type = None;
        let mut raw_data = None;
        for field in fields(tensor) {
            let field = field?;
            match (field.number, field.payload) {
                (TENSOR_DIMS, Payload::Varint(dim)) => dims.push(
                    usize::try_from(dim).map_err(|_| format!("a tensor has dimension {dim}"))?,
                ),
                (TENSOR_DIMS, Payload::Bytes(packed)) => {
                    let mut at = 0;
                    while at < packed.len() {
                        let dim = read_varint(packed, &mut at)?;
                        dims.push(
                            usize::try_from(dim)
                                .map_err(|_| format!("a tensor has dimension {dim}"))?,
                        );
                    }
                }
                (TENSOR_DATA_TYPE, Payload::Varint(kind)) => data_type = Some(kind),
                (TENSOR_RAW_DATA, Payload::Bytes(data)) => raw_data = Some(data),
                _ => {}
            }
        }
        let (Some(DATA_TYPE_FLOAT), Some(data)) = (data_type, raw_data) else {
            return Ok(None);
        };
        let elements: usize = dims.iter().product();
        if data.len() != elements * size_of::<f32>() {
            return Err(format!(
                "a tensor of dimensions {dims:?} holds {} bytes of floats",
                data.len()
            ));
        }
        let (floats, _) = data.as_chunks::<{ size_of::<f32>() }>();
        let values = floats
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect();
        Ok(Some(FloatTensor { dims, values }))
    }

    /// The values of a tensor of `length` values in one dimension.
    fn vector(&self, length: usize) -> Option<&[f32]> {
        (self.dims == [length]).then_some(self.values.as_slice())
    }
}

/// A tensor with `values` as its `raw_data` instead of its own.
fn with_raw_data(tensor: &[u8], values: &[f32]) -> Result<Vec<u8>, String> {
    let mut rewritten = Vec::with_capacity(tensor.len());
    for field in fields(tensor) {
        let field = field?;
        if field.number != TENSOR_RAW_DATA {
            rewritten.extend_from_slice(field.encoded);
        }
    }
    let data: Vec<u8> = values
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect();
    write_bytes_field(&mut rewritten, TENSOR_RAW_DATA, &data);
    Ok(rewritten)
}

fn tensor_name(tensor: &[u8]) -> Result<&str, String> {
    named(tensor, TENSOR_NAME).map_err(|e| format!("an initializer {e}"))
}

fn value_name(value: &[u8]) -> Result<&str, String> {
    named(value, VALUE_INFO_NAME).map_err(|e| format!("a value {e}"))
}

/// The string field `number` of a message, which names it.
fn named(message: &[u8], number: u64) -> Result<&str, String> {
    for field in fields(message) {
        let field = field?;
        if let (true, Payload::Bytes(name)) = (field.number == number, field.payload) {
            return utf8(name);
        }
    }
    Err("has no name".to_owned())
}

fn utf8(bytes: &[u8]) -> Result<&str, String> {
    std::str::from_utf8(bytes).map_err(|e| format!("a string is not UTF-8: {e}"))
}

/// How the nodes of a graph connect: what produces each value and how often it is used.
struct Topology<'g, 'a> {
    nodes: &'g [Node<'a>],
    tensors: &'g HashMap<&'a str, FloatTensor>,
    producers: HashMap<&'a str, usize>,
    uses: HashMap<&'a str, usize>,
}

/// A batch normalization folded into a `Conv`: the `Conv` rewired to read `input`, `weight` and
/// `bias` and write `output`, the values `weight` and `bias` take, and the nodes, values and
/// initializers the fold leaves unused.
struct Fold<'a> {
    conv: usize,
    input: &'a str,
    weight: &'a str,
    bias: &'a str,
    output: &'a str,
    weights: Vec<f32>,
    biases: Vec<f32>,
    nodes: Vec<usize>,
    values: Vec<&'a str>,
    tensors: Vec<&'a str>,
}

impl<'g, 'a> Topology<'g, 'a> {
    fn of(
        nodes: &'g [Node<'a>],
        tensors: &'g HashMap<&'a str, FloatTensor>,
        outputs: &[&'a str],
    ) -> Self {
        let producers = nodes
            .iter()
            .enumerate()
            .flat_map(|(index, node)| node.outputs.iter().map(move |output| (*output, index)))
            .collect();
        let mut uses = HashMap::new();
        for value in nodes.iter().flat_map(|node| &node.inputs).chain(outputs) {
            *uses.entry(*value).or_insert(0) += 1;
        }
        Topology {
            nodes,
            tensors,
            producers,
            uses,
        }
    }

    fn used_once(&self, value: &str) -> bool {
        self.uses.get(value) == Some(&1)
    }

    /// The initializer of 32-bit floats named `value`, if nothing but one input reads it.
    fn constant(&self, value: &str) -> Option<&'g FloatTensor> {
        self.tensors.get(value).filter(|_| self.used_once(value))
    }

    /// The node that produces `value`, through any `Identity` nodes, each value on the way used
    /// once, added to `fold`'s values with those `Identity` nodes to its nodes.
    fn source(&self, mut value: &'a str, fold: &mut Fold<'a>) -> Option<usize> {
        loop {
            if !self.used_once(value) {
                return None;
            }
            let index = *self.producers.get(value)?;
            fold.values.push(value);
            let node = &self.nodes[index];
            if !node.is("Identity") {
                return Some(index);
            }
            let [input] = node.inputs[..] else {
                return None;
            };
            fold.nodes.push(index);
            value = input;
        }
    }

    /// The fold of the node at `index` into the `Conv` before it, if it is a batch normalization
    /// that folds.
    fn fold(&self, index: usize) -> Option<Fold<'a>> {
        let normalization = &self.nodes[index];
        if !normalization.is("BatchNormalization") {
            return None;
        }
        let [normalized, scale, shift, mean, variance] = normalization.inputs[..] else {
            return None;
        };
        let [output] = normalization.outputs[..] else {
            return None;
        };
        let mut fold = Fold {
            conv: 0,
            input: "",
            weight: "",
            bias: shift,
            output,
            weights: Vec::new(),
            biases: Vec::new(),
            nodes: vec![index],
            values: Vec::new(),
            tensors: vec![scale, mean, variance],
        };
        let source = self.source(normalized, &mut fold)?;
        let (conv, conv_bias) = match &self.nodes[source] {
            node if node.is("Conv") => match node.inputs[..] {
                [_, _] | [_, _, ""] => (source, None),
                [_, _, bias] => {
                    fold.tensors.push(bias);
                    (source, Some(bias))
                }
                _ => return None,
            },
            node if node.is("Add") => {
                let [left, right] = node.inputs[..] else {
                    return None;
                };
                let (convolved, bias) = match self.tensors.contains_key(right) {
                    true => (left, right),
                    false => (right, left),
                };
                fold.nodes.push(source);
                let conv = self.source(convolved, &mut fold)?;
                if !self.nodes[conv].is("Conv")
                    || !matches!(self.nodes[conv].inputs[..], [_, _] | [_, _, ""])
                {
                    return None;
                }
                fold.tensors.push(bias);
                (conv, Some(bias))
            }
            _ => return None,
        };
        let [input, weight, ..] = self.nodes[conv].inputs[..] else {
            return None;
        };
        let weights = self.constant(weight)?;
        let (&channels, kernel) = weights.dims.split_first()?;
        let per_channel: usize = kernel.iter().product();
        if per_channel == 0 {
            return None;
        }
        let [scale, shift, mean, variance] =
            [scale, shift, mean, variance].map(|value| self.constant(value)?.vector(channels));
        let (scale, shift, mean, variance) = (scale?, shift?, mean?, variance?);
        let conv_bias = match conv_bias {
            None => None,
            Some(bias) => {
                let bias = self.constant(bias)?;
                let per_channel = match self.nodes[source].is("Add") {
                    true => {
                        bias.dims.len() == weights.dims.len()
                            && channel_count(&bias.dims) == Some(channels)
                    }
                    false => bias.dims == [channels],
                };
                if !per_channel {
                    return None;
                }
                Some(bias.values.as_slice())
            }
        };
        let epsilon = normalization.epsilon.unwrap_or(BATCH_NORMALIZATION_EPSILON);
        let mut folded = weights.values.clone();
        let mut biases = Vec::with_capacity(channels);
        for (channel, channel_weights) in folded.chunks_mut(per_channel).enumerate() {
            let alpha = scale[channel] / (variance[channel] + epsilon).sqrt();
            for weight in channel_weights {
                *weight *= alpha;
            }
            let bias = conv_bias.map_or(0., |bias| bias[channel]);
            biases.push((bias - mean[channel]) * alpha + shift[channel]);
        }
        fold.conv = conv;
        fold.input = input;
        fold.weight = weight;
        fold.weights = folded;
        fold.biases = biases;
        Some(fold)
    }
}

/// The channels of a shape `[1, channels, 1, …]`.
fn channel_count(dims: &[usize]) -> Option<usize> {
    (dims.len() >= 2
        && dims
            .iter()
            .enumerate()
            .all(|(axis, &size)| axis == 1 || size == 1))
    .then(|| dims[1])
}

/// A field of a Protocol Buffers message.
struct Field<'a> {
    number: u64,
    payload: Payload<'a>,
    /// The whole field as it is encoded, its key included.
    encoded: &'a [u8],
}

#[derive(Clone, Copy)]
enum Payload<'a> {
    Bytes(&'a [u8]),
    Varint(u64),
    Fixed32(u32),
    Fixed64,
}

/// The fields of a Protocol Buffers message, in order.
fn fields(message: &[u8]) -> impl Iterator<Item = Result<Field<'_>, String>> {
    let mut at = 0;
    std::iter::from_fn(move || {
        (at < message.len()).then(|| {
            let start = at;
            let key = read_varint(message, &mut at)?;
            let payload = match key & 7 {
                WIRE_VARINT => Payload::Varint(read_varint(message, &mut at)?),
                WIRE_FIXED64 => skip(message, &mut at, 8).map(|_| Payload::Fixed64)?,
                WIRE_FIXED32 => skip(message, &mut at, 4).map(|bytes| {
                    Payload::Fixed32(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
                })?,
                WIRE_LENGTH_DELIMITED => {
                    let length = usize::try_from(read_varint(message, &mut at)?)
                        .map_err(|_| format!("the field at byte {start} is too long"))?;
                    Payload::Bytes(skip(message, &mut at, length)?)
                }
                wire => return Err(format!("the field at byte {start} has wire type {wire}")),
            };
            Ok(Field {
                number: key >> 3,
                payload,
                encoded: &message[start..at],
            })
        })
    })
}

fn skip<'a>(message: &'a [u8], at: &mut usize, length: usize) -> Result<&'a [u8], String> {
    let end = at
        .checked_add(length)
        .filter(|&end| end <= message.len())
        .ok_or_else(|| format!("a field at byte {at} runs past the message's end"))?;
    let bytes = &message[*at..end];
    *at = end;
    Ok(bytes)
}

fn read_varint(message: &[u8], at: &mut usize) -> Result<u64, String> {
    let start = *at;
    let mut value = 0u64;
    for shift in (0..64).step_by(7) {
        let byte = *message
            .get(*at)
            .ok_or_else(|| format!("the varint at byte {start} runs past the message's end"))?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(format!("the varint at byte {start} is longer than 64 bits"))
}

fn write_varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push((value as u8) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn write_bytes_field(out: &mut Vec<u8>, number: u64, bytes: &[u8]) {
    write_varint(out, number << 3 | WIRE_LENGTH_DELIMITED);
    write_varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}

/// The licenses of the native libraries the static build links beyond tesseract's, which
/// `DIJO_STATIC_LICENSES` names as a directory with one subdirectory per library, written
/// into one file.
fn static_licenses(out_dir: &Path) -> PathBuf {
    println!("cargo:rerun-if-env-changed={STATIC_LICENSES_VARIABLE}");
    let dir = PathBuf::from(env::var_os(STATIC_LICENSES_VARIABLE).unwrap_or_else(|| {
        panic!(
            "The static build needs {STATIC_LICENSES_VARIABLE} for the licenses of what it links"
        )
    }));
    println!("cargo:rerun-if-changed={}", dir.display());
    let licenses = out_dir.join("LINKED-LICENSES.md");
    write_licenses(&dir, STATIC_LICENSES_VARIABLE, &licenses);
    licenses
}

/// Writes every license file in `dir`, which holds one subdirectory per component, as one
/// Markdown text; `variable` names where `dir` came from.
fn write_licenses(dir: &Path, variable: &str, target: &Path) {
    let mut licenses = String::new();
    for component in list_dir(dir) {
        for file in list_dir(&component) {
            let text = String::from_utf8_lossy(&read_file(&file)).into_owned();
            licenses.push_str(&format!(
                "## {} ({})\n\n```text\n{}\n```\n\n",
                component.file_name().unwrap().to_string_lossy(),
                file.file_name().unwrap().to_string_lossy(),
                text.trim_end()
            ));
        }
    }
    if licenses.is_empty() {
        panic!(
            "{variable} names {}, which holds no licenses",
            dir.display()
        );
    }
    fs::write(target, licenses)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", target.display()));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    Windows,
    MacOs,
    OtherUnix,
}

impl Platform {
    fn from_target_os(target_os: &str) -> Self {
        match target_os {
            "windows" => Platform::Windows,
            "macos" => Platform::MacOs,
            _ => Platform::OtherUnix,
        }
    }

    /// The bundled tesseract of a build that is not the static executable: the library
    /// ci/ocr-vcpkg.sh builds, which the process loads.
    fn bundled_tesseract(self) -> Tesseract {
        Tesseract::Library(self.dynamic_library(OCR_TESSERACT_LIBRARY_STEM))
    }

    /// Whether the OCR bundle carries heif-png (dijo-native/heif-png): everywhere but
    /// macOS, whose own sips decodes HEIC, HEIF and AVIF.
    fn heif_decoder_bundled(self) -> bool {
        self != Platform::MacOs
    }

    /// The file name of an executable.
    fn executable(self, stem: &str) -> String {
        match self {
            Platform::Windows => format!("{stem}.exe"),
            Platform::MacOs | Platform::OtherUnix => stem.to_string(),
        }
    }

    /// The file name of a dynamic library.
    fn dynamic_library(self, stem: &str) -> String {
        match self {
            Platform::Windows => format!("{stem}.dll"),
            Platform::MacOs | Platform::OtherUnix => {
                format!("lib{stem}.{}", self.dynamic_library_extension())
            }
        }
    }

    fn dynamic_library_extension(self) -> &'static str {
        match self {
            Platform::Windows => "dll",
            Platform::MacOs => "dylib",
            Platform::OtherUnix => "so",
        }
    }
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|e| panic!("Cargo did not provide {name}: {e}"))
}

fn read_file(path: &Path) -> Vec<u8> {
    fs::read(path).unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()))
}

fn bundled_models(bundle: &Path, kind: &str, extension: &str) -> Vec<(String, PathBuf)> {
    let models = models_under(&bundle.join(kind), kind, extension);
    if models.is_empty() {
        panic!(
            "{OCR_BUNDLE_VARIABLE} names {}, which holds no {kind}/**/*.{extension}",
            bundle.display()
        );
    }
    models
}

/// The files under `dir` with `extension`, in subdirectories too (tesseract's
/// script models are in `script/`), each named by its path below `prefix` with `/` separators, in
/// sorted order.
fn models_under(dir: &Path, prefix: &str, extension: &str) -> Vec<(String, PathBuf)> {
    let mut models = Vec::new();
    for path in list_dir(dir) {
        let file = path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_else(|| panic!("OCR model path {} is not UTF-8", path.display()));
        let name = format!("{prefix}/{file}");
        if path.is_dir() {
            models.extend(models_under(&path, &name, extension));
        } else if path.extension() == Some(OsStr::new(extension)) {
            models.push((name, path));
        }
    }
    models
}

/// The entries of a directory, sorted, so that what is built from them does not depend on the
/// order the file system lists them in.
fn list_dir(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("Failed to list {}: {e}", dir.display()))
        .map(|entry| {
            entry
                .unwrap_or_else(|e| panic!("Failed to list {}: {e}", dir.display()))
                .path()
        })
        .collect();
    entries.sort();
    entries
}

fn create_dir(dir: &Path) {
    fs::create_dir_all(dir).unwrap_or_else(|e| panic!("Failed to create {}: {e}", dir.display()));
}

fn remove_dir_if_exists(dir: &Path) {
    match fs::remove_dir_all(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => panic!("Failed to remove {}: {e}", dir.display()),
    }
}

fn write_file(path: &Path, content: &[u8]) {
    fs::write(path, content).unwrap_or_else(|e| panic!("Failed to write {}: {e}", path.display()));
}

fn compress_bytes(content: &[u8], to: &Path) {
    let file =
        fs::File::create(to).unwrap_or_else(|e| panic!("Failed to create {}: {e}", to.display()));
    let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    encoder
        .write_all(content)
        .and_then(|()| encoder.finish())
        .unwrap_or_else(|e| panic!("Failed to compress into {}: {e}", to.display()));
}

/// The module of the static build: only the OCR bundle's language data, since tesseract is
/// linked in, and the licenses of the native libraries linked in.
fn write_static_native_module(out_dir: &Path, ocr: &OcrBundle, linked_licenses: &Path) {
    let module = format!(
        "pub const NATIVE_FINGERPRINT: &str = {:?};\n\
         pub const LINKED_LICENSES: &str = include_str!({linked_licenses:?});\n{}",
        format!("static-ocr-{}", ocr.fingerprint),
        ocr_items(ocr)
    );
    write_if_changed(&out_dir.join("native.rs"), &module);
}

/// The module of a build that links nothing statically: only the OCR bundle.
fn write_ocr_module(out_dir: &Path, ocr: &OcrBundle) {
    let module = format!(
        "pub const NATIVE_FINGERPRINT: &str = {:?};\n{}",
        format!("ocr-{}", ocr.fingerprint),
        ocr_items(ocr)
    );
    write_if_changed(&out_dir.join("native.rs"), &module);
}

/// The items of the generated module that describe the OCR bundle: the tesseract library it
/// carries, unless tesseract is linked in, its heif-png, its language data, its PP-OCR models,
/// each model's weights in a static of their own aligned to `TENSOR_ALIGNMENT`, and its licenses.
fn ocr_items(ocr: &OcrBundle) -> String {
    let ocr_entries: String = ocr
        .files
        .iter()
        .map(|(name, path)| format!("    ({name:?}, include_bytes!({path:?})),\n"))
        .collect();
    let library = ocr
        .tesseract
        .library()
        .map(|library| format!("pub const OCR_TESSERACT_LIBRARY: &str = {library:?};\n"))
        .unwrap_or_default();
    let heif_decoder = ocr
        .heif_decoder
        .as_ref()
        .map(|decoder| format!("pub const OCR_HEIF_DECODER: &str = {decoder:?};\n"))
        .unwrap_or_default();
    let ppocr = &ocr.ppocr;
    let weights: String = ppocr
        .models
        .iter()
        .enumerate()
        .map(|(index, (_, _, weights))| {
            format!(
                "static PPOCR_WEIGHTS_{index}: &Aligned<[u8]> = \
                 &Aligned {{ alignment: [], bytes: *include_bytes!({weights:?}) }};\n"
            )
        })
        .collect();
    let models: String = ppocr
        .models
        .iter()
        .enumerate()
        .map(|(index, (name, graph, _))| {
            format!(
                "    ({name:?}, PpOcrModel {{ graph: include_bytes!({graph:?}), \
                 weights: &PPOCR_WEIGHTS_{index}.bytes }}),\n"
            )
        })
        .collect();
    let configurations: String = ppocr
        .configurations
        .iter()
        .map(|(name, path)| format!("    ({name:?}, include_str!({path:?})),\n"))
        .collect();
    format!(
        "{library}{heif_decoder}\
         pub const OCR_MODELS: &str = {OCR_MODELS:?};\n\
         pub static OCR_FILES: [(&str, &[u8]); {}] = [\n{ocr_entries}];\n\
         pub const PPOCR_WEIGHTS: &str = {PPOCR_WEIGHTS:?};\n\
         #[repr(C, align({TENSOR_ALIGNMENT}))]\n\
         struct Alignment;\n\
         #[repr(C)]\n\
         struct Aligned<Bytes: ?Sized> {{ alignment: [Alignment; 0], bytes: Bytes }}\n\
         {weights}\
         static PPOCR_MODELS: [(&str, PpOcrModel); {}] = [\n{models}];\n\
         static PPOCR_CONFIGURATIONS: [(&str, &str); {}] = [\n{configurations}];\n\
         pub const OCR_LICENSES: &str = include_str!({:?});\n",
        ocr.files.len(),
        ppocr.models.len(),
        ppocr.configurations.len(),
        ocr.licenses
    )
}

fn write_if_changed(path: &Path, content: &str) {
    if fs::read_to_string(path).ok().as_deref() != Some(content) {
        fs::write(path, content)
            .unwrap_or_else(|e| panic!("Failed to write {}: {e}", path.display()));
    }
}
