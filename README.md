# ImageCoordinates

> **Command line utility to gather EXIF location data**

[![Crates.io](https://img.shields.io/crates/v/img_coords?style=flat-square)](https://crates.io/crates/img_coords)
[![Crates.io](https://img.shields.io/crates/d/img_coords?style=flat-square)](https://crates.io/crates/img_coords)
[![License](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE-MIT)

## About
This executable scans a directory tree for images with EXIF GPS data and collects
them into a single file. It can also import a list of paths from STDIN.

Output formats:

- **GeoJSON** (default) — includes altitude as the third coordinate and can embed
  `base64` JPEG thumbnails.
- **KML** — includes timestamps and uses `altitudeMode` so unknown altitudes clamp
  to the ground. KML has no native thumbnail field.
- **GPX** — waypoints with name, elevation, and time.
- **CSV** — `file,latitude,longitude,altitude,direction,timestamp`.
- **HTML** — a self-contained [Leaflet](https://leafletjs.com/) map (tiles load from
  OpenStreetMap, so viewing needs a network connection). Thumbnails are shown when
  enabled.

GeoJSON, GPX, and KML include the capture timestamp and its UTC offset when the
camera recorded one, plus the camera direction and whether it refers to true or
magnetic north.

Supported image extensions are `jpg`/`jpeg`, `png`, `gif`, `tif`/`tiff`, `webp`,
`bmp`, `heic`/`heif`, `avif`, and common TIFF/ISOBMFF-based RAW formats (`dng`,
`cr2`, `cr3`, `nef`, `arw`, `rw2`, `orf`, `raf`, `pef`, …). Coordinates are read
from EXIF, so any format carrying an EXIF block works. Thumbnails are produced for
formats the decoder can read, or from the camera-embedded EXIF thumbnail when one
is present.

## Installation
[Install Rust](https://rust-lang.org/tools/install/) first, if not installed already. Then:
```
cargo install img_coords
```
, _or_ download the binaries for your platform from the release

, _or_ checkout and build the repo manually

### Uninstall
```
cargo uninstall img_coords
```

## Example
Write GeoJSON, with thumbnails, to STDOUT:
```
img_coords scan --dir SOME_ROOT_DIRECTORY --thumbnails
```

Write a KML file, inferring the format from the extension:
```
img_coords scan --dir SOME_ROOT_DIRECTORY -o photos.kml
```

Render a single-file map:
```
img_coords scan --dir SOME_ROOT_DIRECTORY --thumbnails -o photos.html
```

Update existing files, only adding images that are not in them yet:
```
img_coords scan --dir SOME_ROOT_DIRECTORY --update photos.geojson --update photos.kml -o all.json
```

Re-read images that are already present (for example after editing their EXIF data):
```
img_coords scan --dir SOME_ROOT_DIRECTORY --update photos.geojson --refresh -o refreshed.json
```

Limit the result to a region or a date range (date bounds are inclusive):
```
img_coords scan --dir SOME_ROOT_DIRECTORY --bbox 12.0,45.0,13.0,46.0
img_coords scan --dir SOME_ROOT_DIRECTORY --near 45.5,12.3,25 --after 2024-01-01 --before 2024-12-31
```

Drop duplicate photos that share timestamp and GPS data but live under different paths:
```
img_coords scan --dir SOME_ROOT_DIRECTORY --dedupe-by-exif
```

Import a list of file paths from STDIN instead of walking a directory:
```
find SOME_ROOT_DIRECTORY | img_coords import --format kml
```

## Options

- `-o, --output FILE` — write to `FILE` instead of STDOUT (`-` means STDOUT). The
  format is inferred from the extension unless `-f/--format` is given.
- `-f, --format geojson|kml|csv|gpx|html` — output format (default GeoJSON).
- `-s, --sort path|time|none` — deterministic ordering (default `path`).
- `-t, --thumbnails` — embed thumbnails (GeoJSON/HTML only). Thumbnails come from
  the embedded EXIF thumbnail when possible, are oriented, and are size-capped.
- `-b, --before DATE`, `-a, --after DATE` — inclusive `YYYY-MM-DD` date bounds.
- `--bbox MIN_LON,MIN_LAT,MAX_LON,MAX_LAT` — keep only images inside the box.
- `--near LAT,LON,RADIUS_KM` — keep only images within the radius.
- `--dedupe-by-exif` — keep one copy of photos with identical timestamp/GPS EXIF.
- `--refresh` — re-read images already present in the `--update` files.
- `--follow-links` — follow symbolic links while scanning.
- `-q, --quiet` — suppress progress and the summary on stderr.
- `-v, --verbose` — explain why individual files were skipped.

`--dir` and `--update` may be given multiple times. `scan` walks the tree in
parallel and is typically the fastest option; `import` is useful when you already
have a list of paths from another tool.

Run `img_coords` or `img_coords <COMMAND> --help` for the full command reference.
