use crate::file_location::{FileLocation, image_mime};
use anyhow::{Context, Result, anyhow, bail};
use chrono::NaiveDate;
use jwalk::WalkDir;
use jwalk::rayon::prelude::*;
use kml::Kml;
use serde::Deserialize;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::{collections::HashSet, fs};

/// Output format for the collected locations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    #[default]
    #[value(name = "geojson", alias = "json")]
    GeoJson,
    #[value(name = "kml")]
    Kml,
    #[value(name = "csv")]
    Csv,
    #[value(name = "gpx")]
    Gpx,
}

/// How output features are ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum SortKey {
    #[default]
    #[value(name = "path")]
    Path,
    #[value(name = "time")]
    Time,
    #[value(name = "none")]
    None,
}

/// Minimal view of a GeoJSON document, deserialized into typed features so we never
/// have to round-trip each feature through a string.
#[derive(Deserialize)]
struct GeoJsonFeatures {
    features: Vec<geojson::Feature>,
}

/// Number of features rendered in parallel before writing them out.
const OUTPUT_CHUNK: usize = 512;

#[derive(Clone, Debug, Default)]
pub struct FileSet {
    file_locations: Vec<FileLocation>,
    before: Option<NaiveDate>,
    after: Option<NaiveDate>,
}

impl FileSet {
    pub fn set_before(&mut self, date: NaiveDate) {
        self.before = Some(date);
    }

    pub fn set_after(&mut self, date: NaiveDate) {
        self.after = Some(date);
    }

    /// Sorts the collected locations, making output deterministic by default.
    pub fn sort(&mut self, key: SortKey) {
        match key {
            SortKey::Path => self.file_locations.sort_by(|a, b| a.file.cmp(&b.file)),
            SortKey::Time => self.file_locations.sort_by(|a, b| {
                a.timestamp_parsed()
                    .cmp(&b.timestamp_parsed())
                    .then_with(|| a.file.cmp(&b.file))
            }),
            SortKey::None => {}
        }
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
        let document: GeoJsonFeatures = serde_json::from_str(data).context("not valid GeoJSON")?;
        if document.features.is_empty() {
            self.file_locations.clear();
            return Ok(());
        }
        let locations: Vec<FileLocation> = document
            .features
            .par_iter()
            .filter_map(FileLocation::from_feature)
            .collect();
        if locations.is_empty() {
            bail!("no features with a name and Point geometry found");
        }
        self.file_locations = locations;
        Ok(())
    }

    pub fn scan_tree(&mut self, root: &str) -> Result<()> {
        let meta =
            fs::metadata(root).with_context(|| format!("Cannot access directory '{root}'"))?;
        if !meta.is_dir() {
            bail!("'{root}' is not a directory");
        }

        // Resolve the root once instead of calling `canonicalize` for every discovered
        // file; the walk then yields absolute, stable paths that dedup can use as-is.
        let root =
            fs::canonicalize(root).with_context(|| format!("Cannot resolve directory '{root}'"))?;

        // jwalk parallelizes the directory reads internally; we filter by extension while
        // consuming the walk so we only allocate strings for actual image candidates.
        let mut file_candidates = Vec::new();
        let entries = WalkDir::new(&root)
            .try_into_iter()
            .with_context(|| format!("Failed to scan directory tree at '{}'", root.display()))?;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    eprintln!("Warning: skipping unreadable entry: {err}");
                    continue;
                }
            };
            if entry.file_type().is_dir() {
                continue;
            }
            // Check the extension on the file name first so we only build a full path for
            // actual image candidates.
            if image_mime(Path::new(entry.file_name())).is_none() {
                continue;
            }
            let path = entry.path();
            if let Some(path) = path.to_str() {
                file_candidates.push(path.to_string());
            }
        }
        self.add_files(file_candidates, true);
        Ok(())
    }

    pub fn import_files(&mut self) {
        let stdin = io::stdin();
        let file_candidates = io::BufReader::new(stdin)
            .lines()
            .map_while(|line| match line {
                Ok(line) => Some(line),
                Err(err) => {
                    eprintln!("Warning: stopped reading file list: {err}");
                    None
                }
            })
            .collect();
        self.add_files(file_candidates, false);
    }

    /// Filesystem identity of an entry, used for deduplication. `scan` stores canonical
    /// paths while `import` keeps paths as written, so compare on the canonical path and
    /// fall back to the raw path for files that no longer exist.
    fn identity(path: &str) -> PathBuf {
        Path::new(path)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(path))
    }

    /// Adds candidates, skipping ones already present or repeated within the batch.
    /// `candidates_canonical` tells us the paths are already canonical (as produced by
    /// `scan_tree`), so deduplication can avoid a `canonicalize` syscall per file.
    fn add_files(&mut self, file_candidates: Vec<String>, candidates_canonical: bool) {
        let mut seen: HashSet<PathBuf> = self
            .file_locations
            .par_iter()
            .map(|fl| Self::identity(&fl.file))
            .collect();
        let mut candidates: Vec<(PathBuf, String)> = file_candidates
            .into_par_iter()
            .filter(|f| image_mime(Path::new(f.as_str())).is_some())
            .map(|f| {
                let id = if candidates_canonical {
                    PathBuf::from(f.as_str())
                } else {
                    Self::identity(&f)
                };
                (id, f)
            })
            .collect();
        candidates.retain(|(id, _)| seen.insert(id.clone()));

        let mut new_file_locations: Vec<FileLocation> = candidates
            .into_par_iter()
            .filter_map(|(_, f)| FileLocation::from_file(&f))
            .collect();
        if let Some(before) = self.before {
            new_file_locations
                .retain(|fl| fl.timestamp_parsed().is_some_and(|dt| dt.date() <= before));
        }
        if let Some(after) = self.after {
            new_file_locations
                .retain(|fl| fl.timestamp_parsed().is_some_and(|dt| dt.date() >= after));
        }
        self.file_locations.append(&mut new_file_locations);
    }

    pub fn generate_missing_thumbnails(&mut self) {
        // Thumbnailing is CPU-bound (decode + re-encode per image); fan it out.
        self.file_locations
            .par_iter_mut()
            .for_each(|fl| fl.generate_missing_thumbnail());
    }

    pub fn write_to<W: Write>(&self, mut out: W, format: OutputFormat) -> Result<()> {
        match format {
            OutputFormat::Kml => self.write_chunked(
                &mut out,
                concat!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                    "<kml xmlns=\"http://www.opengis.net/kml/2.2\">\n",
                    "<Document>\n",
                ),
                "\n",
                "</Document>\n</kml>\n",
                |location| Ok(location.as_kml()),
            ),
            OutputFormat::GeoJson => self.write_chunked(
                &mut out,
                "{\"type\": \"FeatureCollection\",\"features\": [\n",
                ",\n",
                "\n]}\n",
                |location| Ok(location.as_geojson()?),
            ),
            OutputFormat::Csv => self.write_chunked(
                &mut out,
                "file,latitude,longitude,altitude,direction,timestamp\n",
                "\n",
                "",
                |location| Ok(location.as_csv()),
            ),
            OutputFormat::Gpx => self.write_chunked(
                &mut out,
                concat!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
                    "<gpx version=\"1.1\" creator=\"img_coords\" ",
                    "xmlns=\"http://www.topografix.com/GPX/1/1\">\n",
                ),
                "\n",
                "</gpx>\n",
                |location| Ok(location.as_gpx()),
            ),
        }
    }

    /// Renders features in bounded chunks: each chunk is serialized in parallel, then
    /// written before the next chunk is built, keeping memory use independent of the
    /// total number of features.
    fn write_chunked<W, F>(
        &self,
        out: &mut W,
        header: &str,
        separator: &str,
        footer: &str,
        render: F,
    ) -> Result<()>
    where
        W: Write,
        F: Fn(&FileLocation) -> Result<String> + Sync,
    {
        out.write_all(header.as_bytes())?;
        let mut first = true;
        for chunk in self.file_locations.chunks(OUTPUT_CHUNK) {
            let rendered: Vec<String> = chunk
                .par_iter()
                .map(&render)
                .collect::<Result<Vec<_>, _>>()?;
            for item in rendered {
                if !first {
                    out.write_all(separator.as_bytes())?;
                }
                first = false;
                out.write_all(item.as_bytes())?;
            }
        }
        out.write_all(footer.as_bytes())?;
        out.flush().context("Failed to write output")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_location::FileLocation;
    use chrono::NaiveDate;

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn location(file: &str) -> FileLocation {
        FileLocation {
            file: file.to_string(),
            latitude: crate::file_location::Latitude::new(0.0).unwrap(),
            longitude: crate::file_location::Longitude::new(0.0).unwrap(),
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
        fs.add_files(
            vec![
                "test_files/sunrise.jpg".to_string(), // valid image, exists
                "test_files/sunrise.txt".to_string(), // wrong extension -> filtered
                "test_files/missing.jpg".to_string(), // right extension, does not exist -> dropped
            ],
            false,
        );
        assert_eq!(fs.file_locations.len(), 1);
        assert_eq!(fs.file_locations[0].file, "test_files/sunrise.jpg");
    }

    #[test]
    fn test_add_files_dedups_against_existing() {
        let mut fs = FileSet::default();
        fs.file_locations.push(location("test_files/sunrise.jpg"));
        // Same path offered again: must not be read/added a second time.
        fs.add_files(vec!["test_files/sunrise.jpg".to_string()], false);
        assert_eq!(fs.file_locations.len(), 1);
    }

    #[test]
    fn test_add_files_dedups_within_batch() {
        let mut fs = FileSet::default();
        // The same file listed twice (e.g. duplicated `find` output) is added once.
        fs.add_files(
            vec![
                "test_files/sunrise.jpg".to_string(),
                "test_files/sunrise.jpg".to_string(),
            ],
            false,
        );
        assert_eq!(fs.file_locations.len(), 1);
    }

    #[test]
    fn test_add_files_dedups_relative_and_absolute_spelling() {
        // `import` keeps relative paths while `scan` stores canonical absolute paths;
        // both spellings of the same file must be recognized as one entry.
        let absolute = std::fs::canonicalize("test_files/sunrise.jpg")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let mut fs = FileSet::default();
        fs.file_locations.push(location("test_files/sunrise.jpg"));
        fs.add_files(vec![absolute], false);
        assert_eq!(fs.file_locations.len(), 1);
    }

    #[test]
    fn test_write_to_writer() {
        let mut fs = FileSet::default();
        fs.file_locations.push(location("a.jpg"));
        let mut out = Vec::new();
        fs.write_to(&mut out, OutputFormat::GeoJson).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\"a.jpg\""));
    }

    #[test]
    fn test_sort_path_time_and_none() {
        let mut first = location("b.jpg");
        first.timestamp = Some("2025:01:02 00:00:00".to_string());
        let mut second = location("a.jpg");
        second.timestamp = Some("2025:01:03 00:00:00".to_string());
        let mut third = location("c.jpg");
        third.timestamp = Some("2025:01:01 00:00:00".to_string());
        let mut fs = FileSet {
            file_locations: vec![first, second, third],
            ..Default::default()
        };

        let files = |fs: &FileSet| -> Vec<String> {
            fs.file_locations.iter().map(|l| l.file.clone()).collect()
        };

        fs.sort(SortKey::Path);
        assert_eq!(files(&fs), ["a.jpg", "b.jpg", "c.jpg"]);

        fs.sort(SortKey::Time);
        assert_eq!(files(&fs), ["c.jpg", "b.jpg", "a.jpg"]);

        fs.sort(SortKey::None);
        assert_eq!(files(&fs), ["c.jpg", "b.jpg", "a.jpg"]);
    }

    #[test]
    fn test_before_after_filtering() {
        // sunrise.jpg has EXIF timestamp 2025:03:06.
        let scan = |before: Option<NaiveDate>, after: Option<NaiveDate>| {
            let mut fs = FileSet {
                before,
                after,
                ..Default::default()
            };
            fs.add_files(vec!["test_files/sunrise.jpg".to_string()], false);
            fs.file_locations.len()
        };

        assert_eq!(scan(None, None), 1, "no bounds keeps the image");
        assert_eq!(
            scan(None, Some(date(2020, 1, 1))),
            1,
            "after an earlier date keeps it"
        );
        assert_eq!(
            scan(None, Some(date(2026, 1, 1))),
            0,
            "after a later date drops it"
        );
        assert_eq!(
            scan(Some(date(2026, 1, 1)), None),
            1,
            "before a later date keeps it"
        );
        assert_eq!(
            scan(Some(date(2020, 1, 1)), None),
            0,
            "before an earlier date drops it"
        );
        // The boundary date itself is inclusive: a photo taken during the day is kept
        // even though its timestamp is later than midnight.
        assert_eq!(
            scan(Some(date(2025, 3, 6)), None),
            1,
            "before is inclusive of the whole day"
        );
        assert_eq!(
            scan(None, Some(date(2025, 3, 6))),
            1,
            "after is inclusive of the whole day"
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
        assert!(
            format!("{err:#}").contains("features"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn test_load_from_geojson_rejects_features_without_locations() {
        // Non-empty input that yields no usable entries is an error, not silently empty.
        let data = r#"{"type":"FeatureCollection","features":[
            {"type":"Feature","geometry":{"type":"Point","coordinates":[1,2]},
             "properties":{}}
        ]}"#;
        let mut fs = FileSet::default();
        assert!(fs.load_from_geojson(data).is_err());
    }

    #[test]
    fn test_load_from_geojson_accepts_empty_feature_collection() {
        let data = r#"{"type":"FeatureCollection","features":[]}"#;
        let mut fs = FileSet::default();
        fs.load_from_geojson(data).unwrap();
        assert!(fs.file_locations.is_empty());
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
