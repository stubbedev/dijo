use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use extractous::Extractor;
use std::io::{BufReader, Read};
use std::time::Duration;

/// Benchmark streaming extraction (baseline)
fn extract_to_stream(c: &mut Criterion) {
    let file_path = "../test_files/documents/2022_Q3_AAPL.pdf";
    let extractor = Extractor::new();

    c.bench_function("extract_to_stream_baseline", |b| {
        b.iter(|| {
            // Extract the provided file content to a stream
            let (stream, _) = extractor.extract_file(file_path).unwrap();
            // Because stream implements std::io::Read trait we can perform buffered reading
            // For example we can use it to create a BufReader
            let mut reader = BufReader::new(stream);
            let mut buffer = Vec::new();
            reader.read_to_end(&mut buffer).unwrap();
        })
    });
}

/// Benchmark string extraction (baseline)
fn extract_to_string(c: &mut Criterion) {
    let file_path = "../test_files/documents/2022_Q3_AAPL.pdf";
    let extractor = Extractor::new();

    c.bench_function("extract_to_string_baseline", |b| {
        b.iter(|| {
            // Extract the provided file content to a string
            let _content = extractor.extract_file_to_string(file_path).unwrap();
        })
    });
}

/// Benchmark string extraction with different optimizations
fn extract_to_string_optimizations(c: &mut Criterion) {
    let mut group = c.benchmark_group("string_extraction_optimizations");
    group.measurement_time(Duration::from_secs(10));

    let file_path = "../test_files/documents/2022_Q3_AAPL.pdf";

    // Baseline extractor (minimal optimizations)
    let baseline_extractor = Extractor::new()
        .set_use_mmap(false)
        .set_enable_text_cleaning(false);

    group.bench_function("baseline", |b| {
        b.iter(|| {
            baseline_extractor
                .extract_file_to_string(file_path)
                .unwrap()
        })
    });

    // Buffer-optimized extractor (just buffer improvements)
    let buffer_optimized_extractor = Extractor::new()
        .set_use_mmap(false)
        .set_enable_text_cleaning(false);

    group.bench_function("buffer_optimized", |b| {
        b.iter(|| {
            buffer_optimized_extractor
                .extract_file_to_string(file_path)
                .unwrap()
        })
    });

    // Memory-mapped I/O optimization
    #[cfg(feature = "mmap")]
    {
        let mmap_extractor = Extractor::new()
            .set_use_mmap(true)
            .set_mmap_threshold(0) // Always use mmap
            .set_enable_text_cleaning(false);

        group.bench_function("mmap_enabled", |b| {
            b.iter(|| mmap_extractor.extract_file_to_string(file_path).unwrap())
        });
    }

    // Text cleaning optimization
    let text_cleaning_extractor = Extractor::new()
        .set_use_mmap(false)
        .set_enable_text_cleaning(true);

    group.bench_function("text_cleaning_enabled", |b| {
        b.iter(|| {
            text_cleaning_extractor
                .extract_file_to_string(file_path)
                .unwrap()
        })
    });

    // All optimizations enabled
    let fully_optimized_extractor = Extractor::new()
        .set_use_mmap(true)
        .set_enable_text_cleaning(true);

    group.bench_function("all_optimizations", |b| {
        b.iter(|| {
            fully_optimized_extractor
                .extract_file_to_string(file_path)
                .unwrap()
        })
    });

    group.finish();
}

/// Benchmark different file sizes
fn extract_different_file_sizes(c: &mut Criterion) {
    let mut group = c.benchmark_group("file_size_scaling");
    group.measurement_time(Duration::from_secs(15));

    let test_files = vec![
        ("small", "../test_files/documents/simple.doc"),
        ("medium", "../test_files/documents/2022_Q3_AAPL.pdf"),
        ("large", "../test_files/documents/vodafone.xlsx"),
    ];

    let baseline_extractor = Extractor::new();
    let optimized_extractor = Extractor::new()
        .set_use_mmap(true)
        .set_enable_text_cleaning(true);

    for (size_name, file_path) in test_files {
        // Skip if file doesn't exist
        if !std::path::Path::new(file_path).exists() {
            continue;
        }

        group.bench_with_input(
            BenchmarkId::new("baseline", size_name),
            &file_path,
            |b, path| {
                b.iter(|| baseline_extractor.extract_file_to_string(path).unwrap());
            },
        );

        group.bench_with_input(
            BenchmarkId::new("optimized", size_name),
            &file_path,
            |b, path| {
                b.iter(|| optimized_extractor.extract_file_to_string(path).unwrap());
            },
        );
    }

    group.finish();
}

/// Benchmark text processing optimizations
fn text_processing_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("text_processing");

    // Sample text for processing
    let sample_text = "This is a sample text with\t\tmultiple\n\n\nwhitespace\r\ncharacters and some control\x00characters that need cleaning.".repeat(1000);

    group.bench_function("clean_text_fast", |b| {
        b.iter(|| extractous::clean_text_fast(&sample_text))
    });

    group.bench_function("normalize_whitespace", |b| {
        b.iter(|| extractous::normalize_whitespace(&sample_text))
    });

    group.bench_function("truncate_text_smart", |b| {
        b.iter(|| extractous::truncate_text_smart(&sample_text, 500))
    });

    group.bench_function("text_stats_analysis", |b| {
        b.iter(|| extractous::TextStats::analyze(&sample_text))
    });

    group.finish();
}

/// Benchmark buffer size optimization impact
fn buffer_size_impact(c: &mut Criterion) {
    let mut group = c.benchmark_group("buffer_size_impact");

    let file_path = "../test_files/documents/2022_Q3_AAPL.pdf";

    // Test with original 32KB buffer (simulated by using minimal settings)
    let small_buffer_extractor = Extractor::new()
        .set_use_mmap(false)
        .set_enable_text_cleaning(false);

    group.bench_function("32kb_buffer_equivalent", |b| {
        b.iter(|| {
            small_buffer_extractor
                .extract_file_to_string(file_path)
                .unwrap()
        })
    });

    // Test with optimized 256KB buffer (current default)
    let large_buffer_extractor = Extractor::new()
        .set_use_mmap(false)
        .set_enable_text_cleaning(false);

    group.bench_function("256kb_buffer_optimized", |b| {
        b.iter(|| {
            large_buffer_extractor
                .extract_file_to_string(file_path)
                .unwrap()
        })
    });

    group.finish();
}

/// Benchmark memory mapping threshold optimization
fn mmap_threshold_optimization(c: &mut Criterion) {
    let mut group = c.benchmark_group("mmap_threshold_optimization");

    let file_path = "../test_files/documents/2022_Q3_AAPL.pdf";

    // Test with high threshold (effectively disabled)
    let no_mmap_extractor = Extractor::new()
        .set_use_mmap(true)
        .set_mmap_threshold(10 * 1024 * 1024) // 10MB - higher than test file
        .set_enable_text_cleaning(false);

    group.bench_function("mmap_disabled", |b| {
        b.iter(|| no_mmap_extractor.extract_file_to_string(file_path).unwrap())
    });

    // Test with optimized threshold (512KB)
    let optimized_mmap_extractor = Extractor::new()
        .set_use_mmap(true)
        .set_mmap_threshold(512 * 1024) // 512KB
        .set_enable_text_cleaning(false);

    group.bench_function("mmap_512kb_threshold", |b| {
        b.iter(|| {
            optimized_mmap_extractor
                .extract_file_to_string(file_path)
                .unwrap()
        })
    });

    // Test with aggressive threshold (256KB)
    let aggressive_mmap_extractor = Extractor::new()
        .set_use_mmap(true)
        .set_mmap_threshold(256 * 1024) // 256KB
        .set_enable_text_cleaning(false);

    group.bench_function("mmap_256kb_threshold", |b| {
        b.iter(|| {
            aggressive_mmap_extractor
                .extract_file_to_string(file_path)
                .unwrap()
        })
    });

    group.finish();
}

criterion_group!(
    benches,
    extract_to_stream,
    extract_to_string,
    extract_to_string_optimizations,
    extract_different_file_sizes,
    text_processing_benchmarks,
    buffer_size_impact,
    mmap_threshold_optimization,
);

criterion_main!(benches);
