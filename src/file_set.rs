use crate::file_location::FileLocation;
use anyhow::{Context, Result, anyhow, bail};
use chrono::NaiveDateTime;
use jwalk::WalkDir;
use jwalk::rayon::prelude::*;
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
        let mut locations = Vec::new();
        Self::collect_placemarks(&kml, &mut locations);
        if locations.is_empty() {
            bail!("no placemarks with coordinates found");
        }
        self.file_locations = locations;
        Ok(())
    }

    /// Placemarks in a KML file are typically wrapped in `<Document>`/`<Folder>`
    /// containers (this is how the crate parses them, and how our own KML output is
    /// shaped), so walk the tree rather than only inspecting the top-level elements.
    fn collect_placemarks(element: &Kml, out: &mut Vec<FileLocation>) {
        match element {
            Kml::KmlDocument(doc) => {
                for e in &doc.elements {
                    Self::collect_placemarks(e, out);
                }
            }
            Kml::Document { elements, .. } | Kml::Folder { elements, .. } => {
                for e in elements {
                    Self::collect_placemarks(e, out);
                }
            }
            Kml::Placemark(_) => {
                if let Some(fl) = FileLocation::from_kml_element(element) {
                    out.push(fl);
                }
            }
            _ => {}
        }
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
                let bodies: Vec<String> = self
                    .file_locations
                    .par_iter()
                    .map(|fl| fl.as_kml())
                    .collect();
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
    use crate::file_location::FileLocation;
    use chrono::NaiveDate;

    fn at_midnight(y: i32, m: u32, d: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
    }

    fn location(file: &str) -> FileLocation {
        FileLocation {
            file: file.to_string(),
            latitude: 0.0,
            longitude: 0.0,
            altitude: None,
            direction: None,
            thumbnail: None,
            timestamp: None,
        }
    }

    #[test]
    fn test_scan_tree_finds_images() {
        let mut fs = FileSet::default();
        fs.scan_tree("test_files").unwrap();
        assert_eq!(fs.file_locations.len(), 1);
        assert!(fs.file_locations[0].file.ends_with("sunrise.jpg"));
    }

    #[test]
    fn test_scan_tree_nonexistent_dir_errors() {
        let mut fs = FileSet::default();
        assert!(fs.scan_tree("/no/such/directory/at/all").is_err());
    }

    #[test]
    fn test_scan_tree_file_is_not_a_dir_errors() {
        let mut fs = FileSet::default();
        let err = fs.scan_tree("test_files/sunrise.jpg").unwrap_err();
        assert!(err.to_string().contains("is not a directory"));
    }

    #[test]
    fn test_add_files_filters_extension_and_missing() {
        let mut fs = FileSet::default();
        fs.add_files(vec![
            "test_files/sunrise.jpg".to_string(), // valid image, exists
            "test_files/sunrise.txt".to_string(), // wrong extension -> filtered
            "test_files/missing.jpg".to_string(), // right extension, does not exist -> dropped
        ]);
        assert_eq!(fs.file_locations.len(), 1);
        assert_eq!(fs.file_locations[0].file, "test_files/sunrise.jpg");
    }

    #[test]
    fn test_add_files_dedups_against_existing() {
        let mut fs = FileSet::default();
        fs.file_locations.push(location("test_files/sunrise.jpg"));
        // Same path offered again: must not be read/added a second time.
        fs.add_files(vec!["test_files/sunrise.jpg".to_string()]);
        assert_eq!(fs.file_locations.len(), 1);
    }

    #[test]
    fn test_before_after_filtering() {
        // sunrise.jpg has EXIF timestamp 2025:03:06.
        let scan = |before: Option<NaiveDateTime>, after: Option<NaiveDateTime>| {
            let mut fs = FileSet {
                before,
                after,
                ..Default::default()
            };
            fs.add_files(vec!["test_files/sunrise.jpg".to_string()]);
            fs.file_locations.len()
        };

        assert_eq!(scan(None, None), 1, "no bounds keeps the image");
        assert_eq!(
            scan(None, Some(at_midnight(2020, 1, 1))),
            1,
            "after an earlier date keeps it"
        );
        assert_eq!(
            scan(None, Some(at_midnight(2026, 1, 1))),
            0,
            "after a later date drops it"
        );
        assert_eq!(
            scan(Some(at_midnight(2026, 1, 1)), None),
            1,
            "before a later date keeps it"
        );
        assert_eq!(
            scan(Some(at_midnight(2020, 1, 1)), None),
            0,
            "before an earlier date drops it"
        );
    }

    #[test]
    fn test_load_from_geojson_parses_features() {
        let data = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"Point","coordinates":[12.345,45.6789]},
             "properties":{"name":"one.jpg","altitude":10.0,"direction":90.0}},
            {"type":"Feature","geometry":{"type":"Point","coordinates":[1.0,2.0]},
             "properties":{"name":"two.jpg"}}
        ]}"#;
        let mut fs = FileSet::default();
        fs.load_from_geojson(data).unwrap();
        assert_eq!(fs.file_locations.len(), 2);
        assert_eq!(fs.file_locations[0].file, "one.jpg");
        assert_eq!(fs.file_locations[1].file, "two.jpg");
    }

    #[test]
    fn test_load_from_geojson_rejects_non_json() {
        let mut fs = FileSet::default();
        assert!(fs.load_from_geojson("not json").is_err());
    }

    #[test]
    fn test_load_from_geojson_rejects_missing_features() {
        let mut fs = FileSet::default();
        let err = fs
            .load_from_geojson(r#"{"type":"FeatureCollection"}"#)
            .unwrap_err();
        assert!(err.to_string().contains("features"));
    }

    #[test]
    fn test_load_from_kml_parses_placemarks() {
        let data = r#"<?xml version="1.0" encoding="UTF-8"?>
            <kml xmlns="http://www.opengis.net/kml/2.2"><Document>
            <Placemark><name>a.jpg</name><Point><coordinates>12.345,45.6789,46.79</coordinates></Point></Placemark>
            </Document></kml>"#;
        let mut fs = FileSet::default();
        fs.load_from_kml(data).unwrap();
        assert_eq!(fs.file_locations.len(), 1);
        assert_eq!(fs.file_locations[0].file, "a.jpg");
        assert_eq!(fs.file_locations[0].altitude, Some(46.79));
    }

    #[test]
    fn test_load_from_file_missing_path_errors() {
        let mut fs = FileSet::default();
        let err = fs.load_from_file("/no/such/file.json").unwrap_err();
        assert!(err.to_string().contains("Failed to read"));
    }

    #[test]
    fn test_load_from_file_garbage_reports_both_formats() {
        // A file that is neither GeoJSON nor KML should fail with a message naming both
        // attempts, rather than a misleading single-format error.
        let path = std::env::temp_dir().join("img_coords_test_garbage.dat");
        fs::write(&path, "this is neither geojson nor kml").unwrap();
        let mut fs = FileSet::default();
        let err = fs
            .load_from_file(path.to_str().unwrap())
            .unwrap_err()
            .to_string();
        let _ = fs::remove_file(&path);
        assert!(err.contains("GeoJSON"), "message was: {err}");
        assert!(err.contains("KML"), "message was: {err}");
    }
}
