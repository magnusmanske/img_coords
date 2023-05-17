use std::path::PathBuf;

use file_location::FileLocation;
use glob::glob;
use regex::{Regex, RegexBuilder};
use lazy_static::lazy_static;
use serde_json::json;
use clap::{Parser, Subcommand};

pub mod file_location;

fn scan_tree(root: &str) -> Vec<FileLocation> {
    lazy_static! {
        static ref RE_VALID_FILE_TYPE: Regex = RegexBuilder::new(r"\.(png|gif|tif|tiff|jpg|jpeg)$")
            .case_insensitive(true)
            .build()
            .expect("re_valid_file_type does not compile");
    }

    let path = format!("{root}/**/?*.*");
    glob(&path)
        .expect("glob can't glob path {path}")
        .filter_map(|f|f.ok())
        .filter_map(|f|f.canonicalize().ok())
        .filter_map(|f|f.to_str().map(|f|f.to_string()))
        .filter(|f|RE_VALID_FILE_TYPE.is_match(f))
        .filter_map(|f|FileLocation::from_file(&f))
        .collect()
}

#[derive(Parser)]
#[command(arg_required_else_help = true)]
#[command(name = "ImageCoordinates")]
#[command(author = "Magnus Manske <magnusmanske@gmail.com>")]
#[command(version = "0.1.1")]
#[command(about = "Scans a directory tree for image files with EXIF coordinates and returns a data file", long_about = None)]
struct Cli {

    // /// Turn debugging information on
    // #[arg(short, long, action = clap::ArgAction::Count)]
    // debug: u8,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// scans a directory tree
    Scan {
        /// Specifies the root directory for the tree to scan
        #[arg(short, long, value_name = "FILE")]
        dir: Option<PathBuf>,

        /// Specifies the output format [KML, JSON (default)]
        #[arg(short, long)]
        format: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    match &cli.command {
        Some(Commands::Scan{dir,format}) => {
            let root = match dir {
                Some(dir) => dir.to_str().unwrap(),
                None => ".",
            };
            let file_locations = scan_tree(root);
            match format.to_owned().unwrap_or("json".to_string()).trim().to_lowercase().as_str() {
                "json" => println!("{}",json!(file_locations).to_string()),
                "kml" => {
                    println!(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
                    println!(r#"<kml xmlns="http://www.opengis.net/kml/2.2">"#);
                    for fl in file_locations {
                        println!("{}",fl.as_kml());
                    }
                    println!(r#"</kml>"#);
                }
                other => eprintln!("Unknown format '{other}'"),
            }
            
        },
        None => {}, // Never gets called
    }
}
