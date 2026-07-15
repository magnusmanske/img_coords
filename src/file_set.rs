use crate::file_location::FileLocation;
use anyhow::{anyhow, bail, Context, Result};
use chrono::NaiveDateTime;
use jwalk::rayon::prelude::*;
use jwalk::WalkDir;
use kml::Kml;
use regex::{Regex, RegexBuilder};
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::LazyLock;
use std::{collections::HashSet, fs};

static RE_VALID_FILE_TYPE: LazyLock<Regex> = LazyLock::new(|| {
    RegexBuilder::new(r"\.(png|gif|tif|tiff|jpg|jpeg)$")
        .case_insensitive(true)
        .build()
        .expect("re_valid_file_type does not compile")
});

#[derive(Clone, Debug, Default)]
pub struct FileSet {
    file_locations: Vec<FileLocation>,
    before: Option<NaiveDateTime>,
    after: Option<NaiveDateTime>,
}

impl FileSet {
    pub fn set_before(&mut self, date: NaiveDateTime) {
        self.before = Some(date);
    }

    pub fn set_after(&mut self, date: NaiveDateTime) {
        self.after = Some(date);
    }

    pub fn load_from_file(&mut self, path: &str) -> Result<()> {
        let data =
            fs::read_to_string(path).with_context(|| format!("Failed to read file '{path}'"))?;
        // The file format isn't declared, so try each parser in turn. Keep the reason
        // each one rejected the data so we can report something actionable if none fit.
        let geojson_err = match self.load_from_geojson(&data) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        let kml_err = match self.load_from_kml(&data) {
            Ok(()) => return Ok(()),
            Err(e) => e,
        };
        Err(anyhow!(
            "Could not parse '{path}' as GeoJSON ({geojson_err:#}) or KML ({kml_err:#})"
        ))
    }

    fn load_from_kml(&mut self, data: &str) -> Result<()> {
        let kml: Kml = data.parse().context("not valid KML")?;
        let doc = match kml {
            Kml::KmlDocument(doc) => doc,
            _ => bail!("not a KML document"),
        };
        self.file_locations = doc
            .elements
            .iter()
            .filter_map(FileLocation::from_kml_element)
            .collect();
        if self.file_locations.is_empty() {
            bail!("no placemarks with coordinates found");
        }
        Ok(())
    }

    fn load_from_geojson(&mut self, data: &str) -> Result<()> {
        let res: serde_json::Value = serde_json::from_str(data).context("not valid JSON")?;
        let features = res
            .get("features")
            .context("missing 'features' field")?
            .as_array()
            .context("'features' is not an array")?;
        self.file_locations = features
            .par_iter()
            .filter_map(FileLocation::from_geojson_feature)
            .collect();
        Ok(())
    }

    pub fn scan_tree(&mut self, root: &str) -> Result<()> {
        // jwalk defers access errors to iteration (where they'd be silently dropped by
        // `f.ok()`), so check the root up front to give a clear message for a bad path.
        let meta =
            fs::metadata(root).with_context(|| format!("Cannot access directory '{root}'"))?;
        if !meta.is_dir() {
            bail!("'{root}' is not a directory");
        }

        // The directory walk itself is parallelized internally by jwalk. We collect
        // the raw paths first, then fan out the expensive per-file work (extension
        // filtering + `canonicalize` syscall) across the rayon thread pool. Filtering
        // by extension *before* canonicalizing avoids a syscall for every non-image.
        let paths: Vec<std::path::PathBuf> = WalkDir::new(root)
            // .follow_links(true)
            .try_into_iter()
            .with_context(|| format!("Failed to scan directory tree at '{root}'"))?
            .filter_map(|f| f.ok())
            .map(|f| f.path())
            .collect();

        let file_candidates = paths
            .into_par_iter()
            .filter(|p| Self::has_valid_extension(p))
            .filter_map(|p| p.canonicalize().ok())
            .filter_map(|p| p.to_str().map(|p| p.to_string()))
            .collect();
        self.add_files(file_candidates);
        Ok(())
    }

    fn has_valid_extension(path: &Path) -> bool {
        path.to_str()
            .is_some_and(|s| RE_VALID_FILE_TYPE.is_match(s))
    }

    pub fn import_files(&mut self) {
        let file = io::stdin();
        let file_candidates = io::BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .collect();
        self.add_files(file_candidates);
    }

    fn add_files(&mut self, file_candidates: Vec<String>) {
        let existing: HashSet<String> = self
            .file_locations
            .par_iter()
            .map(|fl| fl.file.to_owned())
            .collect();
        let mut new_file_locations: Vec<FileLocation> = file_candidates
            .par_iter()
            .filter(|f| !existing.contains(*f)) // Not already in set
            .filter(|f| RE_VALID_FILE_TYPE.is_match(f)) // Wrong file ending
            .filter_map(|f| FileLocation::from_file(f))
            .collect();
        if let Some(before) = self.before {
            new_file_locations.retain(|fl| match fl.timestamp_parsed() {
                Some(parsed) => parsed <= before,
                None => false,
            });
        }
        if let Some(after) = self.after {
            new_file_locations.retain(|fl| match fl.timestamp_parsed() {
                Some(date) => date >= after,
                None => false,
            });
        }
        self.file_locations.append(&mut new_file_locations);
    }

    pub fn generate_missing_thumbnails(&mut self) {
        // Thumbnailing is CPU-bound (decode + re-encode per image); fan it out.
        self.file_locations
            .par_iter_mut()
            .for_each(|fl| fl.generate_missing_thumbnail());
    }

    pub fn output(&mut self, format: &Option<String>) -> Result<()> {
        // Lock stdout once and wrap it in a BufWriter: a `println!` per feature would
        // otherwise re-acquire the lock and flush on every line. The per-feature
        // serialization is built in parallel first, since it dominates for large sets.
        let stdout = io::stdout();
        let mut out = io::BufWriter::new(stdout.lock());
        match format
            .to_owned()
            .unwrap_or("geojson".to_string())
            .trim()
            .to_lowercase()
            .as_str()
        {
            "kml" => {
                let bodies: Vec<String> =
                    self.file_locations.par_iter().map(|fl| fl.as_kml()).collect();
                writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#)?;
                writeln!(out, r#"<kml xmlns="http://www.opengis.net/kml/2.2">"#)?;
                writeln!(out, r#"<Document>"#)?;
                for body in &bodies {
                    writeln!(out, "{body}")?;
                }
                writeln!(out, r#"</Document>"#)?;
                writeln!(out, r#"</kml>"#)?;
            }
            "geojson" => {
                let features: Vec<String> = self
                    .file_locations
                    .par_iter()
                    .map(|fl| fl.as_geojson())
                    .collect();
                writeln!(out, r#"{{"type": "FeatureCollection","features": ["#)?;
                let mut comma = "";
                for feature in &features {
                    writeln!(out, "{comma}{feature}")?;
                    comma = ",";
                }
                writeln!(out, r#"]}}"#)?;
            }
            other => bail!("Unknown output format '{other}' (expected 'geojson' or 'kml')"),
        }
        out.flush().context("Failed to write output")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_set() {
        let mut fs = FileSet::default();
        fs.scan_tree("test_files");
        assert!(!fs.file_locations.is_empty());
        // let mut fs = FileSet::new();
        // fs.import_files();
        // assert!(!fs.file_locations.is_empty());
    }
}
