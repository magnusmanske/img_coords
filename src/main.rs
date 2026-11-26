use anyhow::{Context, Result};
use chrono::NaiveDate;
use clap::{Parser, Subcommand, ValueEnum};
use img_coords::file_set::{FileSet, OutputFormat, SortKey};
use img_coords::filter::{Bbox, Near};
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
    about = "Collect EXIF GPS coordinates from images and export them as GeoJSON, KML, CSV, GPX, or a map"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Walk a directory tree and collect every geotagged image
    Scan {
        /// Root directories of the trees to scan (defaults to the current directory)
        #[arg(short, long, value_name = "DIR")]
        dir: Vec<PathBuf>,

        /// Existing GeoJSON/KML files to update; images already in them are skipped
        #[arg(short, long, value_name = "FILE")]
        update: Vec<PathBuf>,

        /// Output file (defaults to STDOUT; use "-" for STDOUT)
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// Output format (inferred from the output extension, else GeoJSON)
        #[arg(short, long, value_enum)]
        format: Option<FormatArg>,

        /// Order of the output features
        #[arg(short = 's', long, value_enum, default_value = "path")]
        sort: SortArg,

        /// Add base64 JPEG thumbnails (GeoJSON/HTML only)
        #[arg(short, long)]
        thumbnails: bool,

        /// Only include images taken on or before this date (YYYY-MM-DD)
        #[arg(short, long, value_name = "DATE")]
        before: Option<String>,

        /// Only include images taken on or after this date (YYYY-MM-DD)
        #[arg(short, long, value_name = "DATE")]
        after: Option<String>,

        /// Keep only images inside MIN_LON,MIN_LAT,MAX_LON,MAX_LAT
        #[arg(long, value_name = "BOUNDS")]
        bbox: Option<String>,

        /// Keep only images within RADIUS_KM of LAT,LON
        #[arg(long, value_name = "LAT,LON,RADIUS_KM")]
        near: Option<String>,

        /// Drop duplicate photos that share timestamp and GPS data
        #[arg(long)]
        dedupe_by_exif: bool,

        /// Re-read images already present in the update file(s)
        #[arg(long)]
        refresh: bool,

        /// Suppress progress and summary output on stderr
        #[arg(short = 'q', long)]
        quiet: bool,

        /// Explain why files were skipped
        #[arg(short = 'v', long)]
        verbose: bool,
    },

    /// Read image paths from STDIN and collect their geotags
    ///
    /// Example: find SOME_DIRECTORY | img_coords import --format kml
    Import {
        /// Existing GeoJSON/KML files to update; images already in them are skipped
        #[arg(short, long, value_name = "FILE")]
        update: Vec<PathBuf>,

        /// Output file (defaults to STDOUT; use "-" for STDOUT)
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,

        /// Output format (inferred from the output extension, else GeoJSON)
        #[arg(short, long, value_enum)]
        format: Option<FormatArg>,

        /// Order of the output features
        #[arg(short = 's', long, value_enum, default_value = "path")]
        sort: SortArg,

        /// Add base64 JPEG thumbnails (GeoJSON/HTML only)
        #[arg(short, long)]
        thumbnails: bool,

        /// Keep only images inside MIN_LON,MIN_LAT,MAX_LON,MAX_LAT
        #[arg(long, value_name = "BOUNDS")]
        bbox: Option<String>,

        /// Keep only images within RADIUS_KM of LAT,LON
        #[arg(long, value_name = "LAT,LON,RADIUS_KM")]
        near: Option<String>,

        /// Drop duplicate photos that share timestamp and GPS data
        #[arg(long)]
        dedupe_by_exif: bool,

        /// Re-read images already present in the update file(s)
        #[arg(long)]
        refresh: bool,

        /// Suppress progress and summary output on stderr
        #[arg(short = 'q', long)]
        quiet: bool,

        /// Explain why files were skipped
        #[arg(short = 'v', long)]
        verbose: bool,
    },
}

/// Options common to both subcommands, after argument parsing.
struct RunOptions {
    format: Option<OutputFormat>,
    output: Option<PathBuf>,
    sort: SortKey,
    thumbnails: bool,
    bbox: Option<String>,
    near: Option<String>,
    dedupe_by_exif: bool,
    quiet: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let mut fs = FileSet::default();
    let options = match cli.command {
        Commands::Scan {
            dir,
            update,
            output,
            format,
            sort,
            thumbnails,
            before,
            after,
            bbox,
            near,
            dedupe_by_exif,
            refresh,
            quiet,
            verbose,
        } => {
            fs.set_quiet(quiet);
            fs.set_verbose(verbose);
            fs.set_refresh(refresh);
            load_update(&mut fs, &update)?;
            if let Some(date) = &before {
                fs.set_before(parse_date(date, "before")?);
            }
            if let Some(date) = &after {
                fs.set_after(parse_date(date, "after")?);
            }
            if dir.is_empty() {
                fs.scan_tree(".")?;
            } else {
                for root in &dir {
                    fs.scan_tree(path_to_str(root)?)?;
                }
            }
            RunOptions {
                format: format.map(OutputFormat::from),
                output,
                sort: SortKey::from(sort),
                thumbnails,
                bbox,
                near,
                dedupe_by_exif,
                quiet,
            }
        }
        Commands::Import {
            update,
            output,
            format,
            sort,
            thumbnails,
            bbox,
            near,
            dedupe_by_exif,
            refresh,
            quiet,
            verbose,
        } => {
            fs.set_quiet(quiet);
            fs.set_verbose(verbose);
            fs.set_refresh(refresh);
            load_update(&mut fs, &update)?;
            fs.import_files();
            RunOptions {
                format: format.map(OutputFormat::from),
                output,
                sort: SortKey::from(sort),
                thumbnails,
                bbox,
                near,
                dedupe_by_exif,
                quiet,
            }
        }
    };

    let format = options
        .format
        .or_else(|| options.output.as_deref().and_then(format_from_extension))
        .unwrap_or_default();

    if let Some(bbox) = options.bbox.as_deref().map(parse_bbox).transpose()? {
        fs.retain(|location| bbox.contains(location.latitude, location.longitude));
    }
    if let Some(near) = options.near.as_deref().map(parse_near).transpose()? {
        fs.retain(|location| near.contains(location.latitude, location.longitude));
    }
    if options.dedupe_by_exif {
        fs.dedupe_by_exif();
    }
    fs.sort(options.sort);

    if options.thumbnails {
        if matches!(format, OutputFormat::GeoJson | OutputFormat::Html) {
            fs.generate_missing_thumbnails();
        } else {
            eprintln!("Warning: --thumbnails only applies to GeoJSON/HTML output; ignoring it");
        }
    }
    if !options.quiet {
        eprintln!("{}", fs.stats().summary());
    }
    write_output(&fs, format, options.output.as_deref())
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
fn load_update(fs: &mut FileSet, updates: &[PathBuf]) -> Result<()> {
    for filename in updates {
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

fn parse_bbox(text: &str) -> Result<Bbox> {
    Bbox::parse(text).with_context(|| {
        format!("Invalid --bbox '{text}', expected MIN_LON,MIN_LAT,MAX_LON,MAX_LAT")
    })
}

fn parse_near(text: &str) -> Result<Near> {
    Near::parse(text)
        .with_context(|| format!("Invalid --near '{text}', expected LAT,LON,RADIUS_KM"))
}

fn path_to_str(path: &Path) -> Result<&str> {
    path.to_str()
        .with_context(|| format!("Path is not valid UTF-8: {path:?}"))
}
