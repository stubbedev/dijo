use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use extractous::Extractor;
use std::io::Read;

const DOCUMENTS: [&str; 5] = [
    "2022_Q3_AAPL.pdf",
    "category-level.docx",
    "vodafone.xlsx",
    "science-exploration-1p.pptx",
    "simple.odt",
];

fn document_path(name: &str) -> String {
    format!("../test_files/documents/{name}")
}

fn extract_to_string(c: &mut Criterion) {
    let extractor = Extractor::new();
    let mut group = c.benchmark_group("extract_file_to_string");
    for name in DOCUMENTS {
        let path = document_path(name);
        group.bench_with_input(BenchmarkId::from_parameter(name), &path, |b, path| {
            b.iter(|| extractor.extract_file_to_string(path).unwrap())
        });
    }
    group.finish();
}

fn extract_to_stream(c: &mut Criterion) {
    let extractor = Extractor::new();
    let mut group = c.benchmark_group("extract_file");
    for name in DOCUMENTS {
        let path = document_path(name);
        group.bench_with_input(BenchmarkId::from_parameter(name), &path, |b, path| {
            b.iter(|| {
                let (mut stream, _) = extractor.extract_file(path).unwrap();
                let mut content = Vec::new();
                stream.read_to_end(&mut content).unwrap();
                content
            })
        });
    }
    group.finish();
}

fn extract_bytes_to_string(c: &mut Criterion) {
    let extractor = Extractor::new();
    let mut group = c.benchmark_group("extract_bytes_to_string");
    for name in DOCUMENTS {
        let bytes = std::fs::read(document_path(name)).unwrap();
        group.bench_with_input(BenchmarkId::from_parameter(name), &bytes, |b, bytes| {
            b.iter(|| extractor.extract_bytes_to_string(bytes).unwrap())
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    extract_to_string,
    extract_to_stream,
    extract_bytes_to_string
);
criterion_main!(benches);
