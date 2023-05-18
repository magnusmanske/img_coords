use std::path::PathBuf;
use file_set::FileSet;
use clap::{Parser, Subcommand};

pub mod file_location;
pub mod file_set;


#[derive(Parser)]
#[command(arg_required_else_help = true)]
#[command(name = "ImageCoordinates")]
#[command(author = "Magnus Manske <magnusmanske@gmail.com>")]
#[command(version = "0.1.2")]
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
    },

    /// imports a list of files from STDIN, eg. `find SOME_DIRECTORY | img_coords`
    Import {
        /// A file (GeoJSON, KML) to update, ignoring files already in the file 
        #[arg(short, long, value_name = "FILE")]
        update: Option<PathBuf>,

        /// Specifies the output format [KML, GEOJSON, JSON (default)]
        #[arg(short, long)]
        format: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    match &cli.command {
        Some(Commands::Scan{dir,update, format}) => {
            let root = match dir {
                Some(dir) => dir.to_str().unwrap(),
                None => ".",
            };
            let mut fs = match update {
                Some(filename) => {
                    let mut fs = FileSet::new();
                    let s = filename.to_str().expect(&format!("Can't convert file name to str: {filename:?}"));
                    fs.load_from_file(s).expect(&format!("Failed to parse original data from file {s}"));
                    fs
                },
                None => FileSet::new(),
            };
            fs.scan_tree(root);
            fs.output(&format);
        },
        Some(Commands::Import{update, format}) => {
            let mut fs = match update {
                Some(filename) => {
                    let mut fs = FileSet::new();
                    let s = filename.to_str().expect(&format!("Can't convert file name to str: {filename:?}"));
                    fs.load_from_file(s).expect(&format!("Failed to parse original data from file {s}"));
                    fs
                },
                None => FileSet::new(),
            };
            fs.import_files();
            fs.output(&format);
        },
        None => {}, // Never gets called
    }
}
