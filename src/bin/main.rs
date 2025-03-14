use chrono::NaiveDate;
use clap::{Parser, Subcommand};
use img_coords::file_set::FileSet;
use std::path::PathBuf;

#[derive(Parser)]
#[command(arg_required_else_help = true)]
#[command(name = "ImageCoordinates")]
#[command(author = "Magnus Manske <magnusmanske@gmail.com>")]
#[command(version = "0.1.4")]
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

        /// A file (GeoJSON, KML) to update, ignoring files already in the file
        #[arg(short, long, value_name = "FILE")]
        update: Option<PathBuf>,

        /// Specifies the output format [KML, GEOJSON]
        #[arg(short, long)]
        format: Option<String>,

        /// Generate thumbnails for GeoJSON
        #[arg(short, long)]
        thumbnails: bool,

        /// Sets a maximum EXIF timestamp (inclusive) YYYY-MM-DD
        #[arg(short, long)]
        before: Option<String>,

        /// Sets a minimum EXIF timestamp (inclusive) YYYY-MM-DD
        #[arg(short, long)]
        after: Option<String>,
    },

    /// imports a list of files from STDIN, eg. `find SOME_DIRECTORY | img_coords`
    Import {
        /// A file (GeoJSON, KML) to update, ignoring files already in the file
        #[arg(short, long, value_name = "FILE")]
        update: Option<PathBuf>,

        /// Specifies the output format [KML, GEOJSON, JSON (default)]
        #[arg(short, long)]
        format: Option<String>,

        /// Generate thumbnails for GeoJSON
        #[arg(short, long)]
        thumbnails: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    match &cli.command {
        Some(Commands::Scan {
            dir,
            update,
            format,
            thumbnails,
            before,
            after,
        }) => {
            let root = match dir {
                Some(dir) => dir.to_str().unwrap(),
                None => ".",
            };
            let mut fs = FileSet::default();
            if let Some(filename) = update {
                let s = filename
                    .to_str()
                    .expect("Can't convert file name to str: {filename:?}");
                fs.load_from_file(s)
                    .expect("Failed to parse original data from file {s}");
            }
            const DATE_FORMAT: &str = "%Y-%m-%d";
            if let Some(date) = before {
                fs.set_before(
                    NaiveDate::parse_from_str(date, DATE_FORMAT)
                        .expect("Bad date: before")
                        .and_hms_opt(0, 0, 0)
                        .unwrap(),
                );
            }
            if let Some(date) = after {
                fs.set_after(
                    NaiveDate::parse_from_str(date, DATE_FORMAT)
                        .expect("Bad date: after")
                        .and_hms_opt(0, 0, 0)
                        .unwrap(),
                );
            }
            fs.scan_tree(root);
            if *thumbnails {
                fs.generate_missing_thumbnails();
            }
            fs.output(format);
        }
        Some(Commands::Import {
            update,
            format,
            thumbnails,
        }) => {
            let mut fs = FileSet::default();
            if let Some(filename) = update {
                let s = filename
                    .to_str()
                    .expect("Can't convert file name to str: {filename:?}");
                fs.load_from_file(s)
                    .expect("Failed to parse original data from file {s}");
            }
            fs.import_files();
            if *thumbnails {
                fs.generate_missing_thumbnails();
            }
            fs.output(format);
        }
        None => {} // Never gets called
    }
}
