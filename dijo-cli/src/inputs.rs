use std::path::{Component, Path, PathBuf};

use walkdir::WalkDir;

const STDIN: &str = "-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Stdin,
    File(PathBuf),
}

/// One document to process, with the path it is reported under and the path its output takes
/// below an output directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub source: Source,
    pub display: String,
    pub output_path: PathBuf,
}

/// An input argument that names no document, reported in place of the documents it would have
/// produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputError {
    pub display: String,
    pub message: String,
}

pub fn expand(arguments: &[PathBuf], recursive: bool) -> Vec<Result<Input, InputError>> {
    let mut inputs = Vec::new();
    for argument in arguments {
        if argument.as_os_str() == STDIN {
            inputs.push(Ok(Input {
                source: Source::Stdin,
                display: STDIN.to_string(),
                output_path: PathBuf::from("stdin"),
            }));
        } else if argument.is_dir() {
            if recursive {
                walk(argument, &mut inputs);
            } else {
                inputs.push(Err(InputError {
                    display: argument.display().to_string(),
                    message: "is a directory; pass --recursive to extract what it contains"
                        .to_string(),
                }));
            }
        } else {
            inputs.push(Ok(file_input(argument.clone())));
        }
    }
    inputs
}

fn walk(dir: &Path, inputs: &mut Vec<Result<Input, InputError>>) {
    for entry in WalkDir::new(dir).sort_by_file_name() {
        match entry {
            Ok(entry) if entry.file_type().is_file() => {
                inputs.push(Ok(file_input(entry.into_path())));
            }
            Ok(_) => {}
            Err(error) => inputs.push(Err(InputError {
                display: error.path().unwrap_or(dir).display().to_string(),
                message: error.to_string(),
            })),
        }
    }
}

fn file_input(path: PathBuf) -> Input {
    Input {
        display: path.display().to_string(),
        output_path: output_path(&path),
        source: Source::File(path),
    }
}

fn output_path(path: &Path) -> PathBuf {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_paths_stay_below_the_output_directory() {
        assert_eq!(
            output_path(Path::new("/srv/docs/a.pdf")),
            Path::new("srv/docs/a.pdf")
        );
        assert_eq!(
            output_path(Path::new("../x/./b.docx")),
            Path::new("x/b.docx")
        );
        assert_eq!(output_path(Path::new("c.odt")), Path::new("c.odt"));
    }
}
