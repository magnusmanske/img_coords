use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::{Parser, Subcommand};
use img_coords::file_set::{FileSet, OutputFormat, SortKey};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    arg_required_else_help = true,
    author = "Magnus Manske <magnusmanske@gmail.com>",
    version,
    about = "Collect EXIF GPS coordinates from images and export them as GeoJSON or KML"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Walk a directory tree and collect every geotagged image
    Scan {
        /// Root directory of the tree to scan (defaults to the current directory)
        #[arg(short, long, value_name = "DIR")]
        dir: Option<PathBuf>,

        /// Existing GeoJSON/KML file to update; images already in it are skipped
        #[arg(short, long, value_name = "FILE")]
        update: Option<PathBuf>,

        /// Output format
        #[arg(short, long, value_enum, default_value = "geojson")]
        format: OutputFormat,

        /// Order of the output features
        #[arg(short = 's', long, value_enum, default_value = "path")]
        sort: SortKey,

        /// Add base64 JPEG thumbnails (GeoJSON only)
        #[arg(short, long)]
        thumbnails: bool,

        /// Only include images taken on or before this date (YYYY-MM-DD)
        #[arg(short, long, value_name = "DATE")]
        before: Option<String>,

        /// Only include images taken on or after this date (YYYY-MM-DD)
        #[arg(short, long, value_name = "DATE")]
        after: Option<String>,
    },

    /// Read image paths from STDIN and collect their geotags
    ///
    /// Example: find SOME_DIRECTORY | img_coords import --format kml
    Import {
        /// Existing GeoJSON/KML file to update; images already in it are skipped
        #[arg(short, long, value_name = "FILE")]
        update: Option<PathBuf>,

        /// Output format
        #[arg(short, long, value_enum, default_value = "geojson")]
        format: OutputFormat,

        /// Order of the output features
        #[arg(short = 's', long, value_enum, default_value = "path")]
        sort: SortKey,

        /// Add base64 JPEG thumbnails (GeoJSON only)
        #[arg(short, long)]
        thumbnails: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut fs = FileSet::default();
    let (format, thumbnails, sort) = match cli.command {
        Commands::Scan {
            dir,
            update,
            format,
            sort,
            thumbnails,
            before,
            after,
        } => {
            let root = match &dir {
                Some(dir) => path_to_str(dir)?,
                None => ".",
            };
            load_update(&mut fs, &update)?;
            if let Some(date) = &before {
                fs.set_before(parse_date(date, "before")?);
            }
            if let Some(date) = &after {
                fs.set_after(parse_date(date, "after")?);
            }
            fs.scan_tree(root)?;
            (format, thumbnails, sort)
        }
        Commands::Import {
            update,
            format,
            sort,
            thumbnails,
        } => {
            load_update(&mut fs, &update)?;
            fs.import_files();
            (format, thumbnails, sort)
        }
    };

    fs.sort(sort);

    if thumbnails {
        if format == OutputFormat::GeoJson {
            fs.generate_missing_thumbnails();
        } else {
            eprintln!("Warning: --thumbnails only applies to GeoJSON output; ignoring it for KML");
        }
    }
    fs.output(format)
}

/// Loads an existing GeoJSON/KML file into the set so its entries can be updated.
fn load_update(fs: &mut FileSet, update: &Option<PathBuf>) -> Result<()> {
    if let Some(filename) = update {
        let s = path_to_str(filename)?;
        fs.load_from_file(s)
            .with_context(|| format!("Failed to load update file '{s}'"))?;
    }
    Ok(())
}

fn parse_date(date: &str, which: &str) -> Result<NaiveDate> {
    const DATE_FORMAT: &str = "%Y-%m-%d";
    NaiveDate::parse_from_str(date, DATE_FORMAT)
        .with_context(|| format!("Invalid --{which} date '{date}', expected YYYY-MM-DD"))
}

fn path_to_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("Path is not valid UTF-8: {path:?}"))
}
