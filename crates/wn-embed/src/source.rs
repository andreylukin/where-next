//! Where model files come from, and how their names and URLs are formed.
//!
//! A source is parsed from one string so the CLI can take `--source <…>`:
//!
//! - `hf:owner/repo` or `hf:owner/repo@revision`: a Hugging Face model repository (revision
//!   defaults to `main`). A token in `$HF_TOKEN` is sent for gated repositories.
//! - `https://…` (or `http://…`): a base URL; files are fetched as `<base>/<file>`.
//! - anything else: a local directory.
//!
//! Every source must provide `wn-manifest.json`; only the files it lists are fetched, and only
//! after each name has been checked with [`safe_file_name`].

use std::path::{Component, Path, PathBuf};

/// Where model files come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// A local directory containing the model files and a `wn-manifest.json`.
    LocalDir(PathBuf),
    /// A base URL: files are fetched as `<base>/<file>`.
    Url(String),
    /// A Hugging Face model repository at a revision.
    HuggingFace { repo: String, revision: String },
}

/// Why a source string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError(pub String);

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid model source: {}", self.0)
    }
}

impl std::error::Error for SourceError {}

impl ModelSource {
    /// Parses `hf:owner/repo[@revision]`, an `http(s)://` base URL, or a local path.
    pub fn parse(text: &str) -> Result<Self, SourceError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(SourceError("empty".into()));
        }
        if let Some(rest) = text.strip_prefix("hf:") {
            let (repo, revision) = match rest.split_once('@') {
                Some((repo, rev)) => (repo, rev),
                None => (rest, "main"),
            };
            let valid_part = |s: &str| {
                !s.is_empty()
                    && s != "."
                    && s != ".."
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
            };
            let parts: Vec<&str> = repo.split('/').collect();
            if parts.len() != 2 || !parts.iter().all(|p| valid_part(p)) {
                return Err(SourceError(format!("expected hf:owner/repo, got {text:?}")));
            }
            if revision.is_empty()
                || !revision
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
            {
                return Err(SourceError(format!("bad revision in {text:?}")));
            }
            return Ok(ModelSource::HuggingFace {
                repo: repo.to_string(),
                revision: revision.to_string(),
            });
        }
        if text.starts_with("https://") || text.starts_with("http://") {
            return Ok(ModelSource::Url(text.trim_end_matches('/').to_string()));
        }
        Ok(ModelSource::LocalDir(PathBuf::from(text)))
    }

    /// The URL of one file for remote sources; `None` for local directories.
    pub fn file_url(&self, file: &str) -> Option<String> {
        match self {
            ModelSource::LocalDir(_) => None,
            ModelSource::Url(base) => Some(format!("{}/{file}", base.trim_end_matches('/'))),
            ModelSource::HuggingFace { repo, revision } => Some(format!(
                "https://huggingface.co/{repo}/resolve/{revision}/{file}"
            )),
        }
    }

    /// Short human-readable description.
    pub fn describe(&self) -> String {
        match self {
            ModelSource::LocalDir(p) => format!("local directory {}", p.display()),
            ModelSource::Url(u) => u.clone(),
            ModelSource::HuggingFace { repo, revision } => format!("hf:{repo}@{revision}"),
        }
    }
}

/// A manifest file name is safe when it is a relative path of normal components (no `..`, no
/// absolute paths, no drive prefixes, no empty names). Remote manifests are untrusted input.
pub fn safe_file_name(name: &str) -> bool {
    if name.is_empty() || name.contains('\\') || name.contains('\0') {
        return false;
    }
    let path = Path::new(name);
    path.components().count() > 0 && path.components().all(|c| matches!(c, Component::Normal(_)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_huggingface_with_and_without_revision() {
        assert_eq!(
            ModelSource::parse("hf:acme/wn-model").unwrap(),
            ModelSource::HuggingFace {
                repo: "acme/wn-model".into(),
                revision: "main".into()
            }
        );
        assert_eq!(
            ModelSource::parse("hf:acme/wn-model@v1.2").unwrap(),
            ModelSource::HuggingFace {
                repo: "acme/wn-model".into(),
                revision: "v1.2".into()
            }
        );
    }

    #[test]
    fn rejects_malformed_huggingface_ids() {
        for bad in [
            "hf:",
            "hf:acme",
            "hf:a/b/c",
            "hf:../x",
            "hf:acme/..",
            "hf:acme/x@",
            "hf:acme/x@a b",
            "hf:ac me/x",
        ] {
            assert!(ModelSource::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parses_urls_and_paths() {
        assert_eq!(
            ModelSource::parse("https://example.com/m/").unwrap(),
            ModelSource::Url("https://example.com/m".into())
        );
        assert_eq!(
            ModelSource::parse("/tmp/model").unwrap(),
            ModelSource::LocalDir("/tmp/model".into())
        );
        assert!(ModelSource::parse("  ").is_err());
    }

    #[test]
    fn file_urls() {
        let hf = ModelSource::parse("hf:acme/m@r1").unwrap();
        assert_eq!(
            hf.file_url("model.onnx").unwrap(),
            "https://huggingface.co/acme/m/resolve/r1/model.onnx"
        );
        let url = ModelSource::parse("https://x.test/base").unwrap();
        assert_eq!(
            url.file_url("a.json").unwrap(),
            "https://x.test/base/a.json"
        );
        assert!(ModelSource::LocalDir("/x".into()).file_url("a").is_none());
    }

    #[test]
    fn safe_names() {
        for ok in ["model.onnx", "sub/tokenizer.json"] {
            assert!(safe_file_name(ok), "{ok}");
        }
        for bad in ["", "../etc/passwd", "/abs", "a/../b", "./x", "a\\b", "x\0y"] {
            assert!(!safe_file_name(bad), "{bad:?}");
        }
    }
}
