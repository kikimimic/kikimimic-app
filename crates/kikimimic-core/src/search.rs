//! Local literal search. Each call reads current sources; no persistent index or network.
use std::path::{Path, PathBuf};

#[derive(Debug, serde::Serialize)]
pub struct Hit {
    pub source: PathBuf,
    pub title: String,
    pub started: Option<String>,
    pub start_ms: Option<u64>,
    pub text: String,
}

/// Search recordings and only immediate Markdown/text children of explicitly
/// supplied folders. Symlinks are skipped. Empty queries return no results.
/// Matching ignores letter case, but deliberately does not infer project identity
/// or normalize spelling variants. Refreshing naturally reflects edits/deletions.
pub fn search(base: &Path, folders: &[PathBuf], query: &str) -> anyhow::Result<Vec<Hit>> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let matches = |s: &str| s.to_lowercase().contains(&query);
    let mut hits = Vec::new();
    for recording in crate::library::list(base) {
        let path = PathBuf::from(&recording.dir);
        if path.symlink_metadata()?.file_type().is_symlink() {
            continue;
        }
        let session = crate::library::read(&path)?;
        let title = session.title.unwrap_or(session.name);
        let metadata_match = matches(&title) || session.started.as_deref().is_some_and(&matches);
        if metadata_match {
            hits.push(Hit {
                source: path.clone(),
                title: title.clone(),
                started: session.started.clone(),
                start_ms: None,
                text: title.clone(),
            });
        }
        for line in session.lines {
            if matches(&line.text) || line.translation.as_deref().is_some_and(&matches) {
                hits.push(Hit {
                    source: path.clone(),
                    title: title.clone(),
                    started: session.started.clone(),
                    start_ms: Some(line.start_ms),
                    text: line
                        .translation
                        .map_or(line.text.clone(), |t| format!("{}\n{}", line.text, t)),
                });
            }
        }
    }
    for folder in folders {
        for entry in std::fs::read_dir(folder)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            if !matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("md" | "txt")
            ) {
                continue;
            }
            let text = std::fs::read_to_string(&path)?;
            for line in text.lines().filter(|line| matches(line)) {
                hits.push(Hit {
                    source: path.clone(),
                    title: entry.file_name().to_string_lossy().into_owned(),
                    started: None,
                    start_ms: None,
                    text: line.to_string(),
                });
            }
        }
    }
    hits.sort_by(|a, b| a.source.cmp(&b.source).then(a.start_ms.cmp(&b.start_ms)));
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_japanese_and_refresh() {
        let root = std::env::temp_dir().join(format!("kkm-search-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("memo.md");
        std::fs::write(&file, "検索機で案件ACMEの宿題を確認").unwrap();
        for query in ["検", "検索", "検索機", "acme", "宿題"] {
            assert_eq!(
                search(&root.join("missing"), std::slice::from_ref(&root), query)
                    .unwrap()
                    .len(),
                1
            );
        }
        assert!(search(&root, &[], "宿題").unwrap().is_empty());
        std::fs::write(&file, "完了").unwrap();
        assert!(search(&root, std::slice::from_ref(&root), "宿題").unwrap().is_empty());
        std::fs::remove_file(file).unwrap();
        assert!(search(&root, std::slice::from_ref(&root), "完了").unwrap().is_empty());
        std::fs::remove_dir(root).unwrap();
    }
}
