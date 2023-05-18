use jwalk::rayon::prelude::*;
use std::{fs::{self}, error::Error, collections::HashSet};
use std::io::{ self, BufRead};
use jwalk::WalkDir;
use regex::{Regex, RegexBuilder};
use lazy_static::lazy_static;
use kml::Kml;
use crate::file_location::FileLocation;

#[derive(Clone, Debug)]
pub struct FileSet {
    file_locations: Vec<FileLocation>,
}

impl FileSet {
    pub fn new() -> Self {
        Self {
            file_locations: vec![],
        }
    }
    
    pub fn load_from_file(&mut self, path: &str) -> Result<(),Box<dyn Error>> {
        if let Ok(res) = self.load_from_geojson(path) {
            return Ok(res)
        }
        if let Ok(res) = self.load_from_kml(path) {
            return Ok(res)
        }
        Err(format!("Could not find valid format of file {path}").into())
    }

    fn load_from_kml(&mut self, path: &str) -> Result<(),Box<dyn Error>> {
        let kml_str: String = fs::read_to_string(path)?.parse()?;
        let kml: Kml = kml_str.parse().unwrap();
        let doc = match kml {
            Kml::KmlDocument(doc) => doc,
            _ => return Err("Not a KML document".into()),
        };
        self.file_locations = doc.elements
            .iter()
            .filter_map(|element|FileLocation::from_kml_element(element))
            .collect();
        if self.file_locations.is_empty() {
            return Err("No results from KML".into());
        }
        Ok(())
    }

    fn load_from_geojson(&mut self, path: &str) -> Result<(),Box<dyn Error>> {
        let data = fs::read_to_string(path)?;
        let res: serde_json::Value = serde_json::from_str(&data)?;
        let features = res.get("features").unwrap().as_array().unwrap();
        self.file_locations = features
            .iter()
            .filter_map(|feature|FileLocation::from_geojson_feature(feature))
            .collect();
        Ok(())
    }

    pub fn scan_tree(&mut self, root: &str) {
        let iterator = WalkDir::new(root)
            // .follow_links(true)
            .try_into_iter()
            .expect("Directory walker failed");

        let file_candidates = iterator
            .filter_map(|f|f.ok())
            .map(|f|f.path())
            .filter_map(|f|f.canonicalize().ok())
            .filter_map(|f|f.to_str().map(|f|f.to_string()))
            .collect();
        self.add_files(file_candidates);
    }

    pub fn import_files(&mut self) {
        let file = io::stdin();
        let file_candidates = io::BufReader::new(file).lines().filter_map(|line|line.ok()).collect();
        self.add_files(file_candidates);
    }

    fn add_files(&mut self, file_candidates: Vec<String>) {
        lazy_static! {
            static ref RE_VALID_FILE_TYPE: Regex = RegexBuilder::new(r"\.(png|gif|tif|tiff|jpg|jpeg)$")
                .case_insensitive(true)
                .build()
                .expect("re_valid_file_type does not compile");
        }
        let existing : HashSet<String> = self.file_locations.par_iter().map(|fl|fl.file.to_owned()).collect();
        let mut new_file_locations: Vec<_> = file_candidates
            .par_iter()
            .filter(|f|!existing.contains(*f)) // Not already in set
            .filter(|f|RE_VALID_FILE_TYPE.is_match(f)) // Wrong file ending
            .filter_map(|f|FileLocation::from_file(&f))
            .collect();
        self.file_locations.append(&mut new_file_locations);
    }

    pub fn output(&self, format: &Option<String>) {
        match format.to_owned().unwrap_or("geojson".to_string()).trim().to_lowercase().as_str() {
            "kml" => {
                println!(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
                println!(r#"<kml xmlns="http://www.opengis.net/kml/2.2">"#);
                for fl in &self.file_locations {
                    println!("{}",fl.as_kml());
                }
                println!(r#"</kml>"#);
            }
            "geojson" => {
                let mut comma = String::new();
                println!("{}",r#"{"type": "FeatureCollection","features": ["#);
                for fl in &self.file_locations {
                    println!("{comma}{}",fl.as_geojson());
                    if comma.is_empty() {
                        comma = ",".into();
                    }
                }
                println!("{}",r#"]}"#);
            }
            other => eprintln!("Unknown format '{other}'"),
        }
    }
}