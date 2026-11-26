use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::{Parser, Subcommand, ValueEnum};
use img_coords::file_set::{FileSet, OutputFormat, SortKey};
use std::path::{Path, PathBuf};

/// Command-line wrapper around [`OutputFormat`].
#[derive(Clone, Copy, Debug, ValueEnum)]
enum FormatArg {
    #[value(name = "geojson", alias = "json")]
    GeoJson,
    Kml,
    Csv,
    Gpx,
    Html,
}

impl From<FormatArg> for OutputFormat {
    fn from(value: FormatArg) -> Self {
        match value {
            FormatArg::GeoJson => Self::GeoJson,
            FormatArg::Kml => Self::Kml,
            FormatArg::Csv => Self::Csv,
            FormatArg::Gpx => Self::Gpx,
            FormatArg::Html => Self::Html,
        }
    }
}

/// Command-line wrapper around [`SortKey`].
#[derive(Clone, Copy, Debug, ValueEnum)]
enum SortArg {
    #[value(name = "path")]
    Path,
    #[value(name = "time")]
    Time,
    #[value(name = "none")]
    Preserve,
}

impl From<SortArg> for SortKey {
    fn from(value: SortArg) -> Self {
        match value {
            SortArg::Path => Self::Path,
            SortArg::Time => Self::Time,
            SortArg::Preserve => Self::None,
        }
    }
}

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

        /// Output file (defaults to STDOUT; use "-" for STDOUT)
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// Output format (inferred from the output extension, else GeoJSON)
        #[arg(short, long, value_enum)]
        format: Option<FormatArg>,

        /// Order of the output features
        #[arg(short = 's', long, value_enum, default_value = "path")]
        sort: SortArg,

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

        /// Output file (defaults to STDOUT; use "-" for STDOUT)
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// Output format (inferred from the output extension, else GeoJSON)
        #[arg(short, long, value_enum)]
        format: Option<FormatArg>,

        /// Order of the output features
        #[arg(short = 's', long, value_enum, default_value = "path")]
        sort: SortArg,

        /// Add base64 JPEG thumbnails (GeoJSON only)
        #[arg(short, long)]
        thumbnails: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut fs = FileSet::default();
    let (format, thumbnails, sort, output) = match cli.command {
        Commands::Scan {
            dir,
            update,
            output,
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
            (format, thumbnails, sort, output)
        }
        Commands::Import {
            update,
            output,
            format,
            sort,
            thumbnails,
        } => {
            load_update(&mut fs, &update)?;
            fs.import_files();
            (format, thumbnails, sort, output)
        }
    };

    let format = format
        .map(OutputFormat::from)
        .or_else(|| output.as_deref().and_then(format_from_extension))
        .unwrap_or_default();

    fs.sort(SortKey::from(sort));

    if thumbnails {
        if matches!(format, OutputFormat::GeoJson | OutputFormat::Html) {
            fs.generate_missing_thumbnails();
        } else {
            eprintln!("Warning: --thumbnails only applies to GeoJSON/HTML output; ignoring it");
        }
    }
    write_output(&fs, format, output.as_deref())
}

/// Writes the collected locations to `output`, or to STDOUT when no file (or `-`) is given.
fn write_output(fs: &FileSet, format: OutputFormat, output: Option<&Path>) -> Result<()> {
    match output {
        Some(path) if path != Path::new("-") => {
            let file = std::fs::File::create(path)
                .with_context(|| format!("Failed to create output file '{path:?}'"))?;
            let mut out = std::io::BufWriter::new(file);
            fs.write_to(&mut out, format)
        }
        _ => {
            let stdout = std::io::stdout();
            let mut out = std::io::BufWriter::new(stdout.lock());
            fs.write_to(&mut out, format)
        }
    }
}

/// Infers the output format from a file extension.
fn format_from_extension(path: &Path) -> Option<OutputFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "kml" => Some(OutputFormat::Kml),
        "csv" => Some(OutputFormat::Csv),
        "gpx" => Some(OutputFormat::Gpx),
        "html" | "htm" => Some(OutputFormat::Html),
        "json" | "geojson" => Some(OutputFormat::GeoJson),
        _ => None,
    }
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
