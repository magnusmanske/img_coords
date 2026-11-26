# ImageCoordinates

> **Command line utility to gather EXIF location data**

[![Crates.io](https://img.shields.io/crates/v/img_coords?style=flat-square)](https://crates.io/crates/img_coords)
[![Crates.io](https://img.shields.io/crates/d/img_coords?style=flat-square)](https://crates.io/crates/img_coords)
[![License](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE-MIT)

## About
This executable can scan a directory tree on your disk, find all images with EXIF data containing coordinates, and collect them into a single file.
Output can be either [GeoJSON](https://geojson.org/) (default) or KML. Both formats include the timestamp the image was taken and the camera direction, if available in EXIF.
GeoJSON also stores altitude as the point's third coordinate and can embed `base64`-encoded JPEG thumbnails.

Supported image extensions are `jpg`/`jpeg`, `png`, `gif`, `tif`/`tiff`, `webp`, `bmp`, `heic`/`heif`, and `avif`.
Coordinates are read from EXIF, so any format carrying an EXIF block works; thumbnails are only produced for formats the image decoder can read (typically not HEIC/HEIF/AVIF).

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
Write GeoJSON, with thumbnails:
```
img_coords scan --dir SOME_ROOT_DIRECTORY --thumbnails > images.json
```

Write a KML file:
```
img_coords scan --dir SOME_ROOT_DIRECTORY --format kml  > images.kml
```

Use a previously generated GeoJSON/KML file and only add files not in there:
```
img_coords scan --dir SOME_ROOT_DIRECTORY --update images.kml
```

Limit the result to a date range (both bounds are inclusive, `YYYY-MM-DD`):
```
img_coords scan --dir SOME_ROOT_DIRECTORY --after 2024-01-01 --before 2024-12-31 > images.json
```

Import a list of file paths from STDIN instead of walking a directory (e.g. via `find`):
```
find SOME_ROOT_DIRECTORY | img_coords import --format kml > images.kml
```

`--thumbnails` applies to GeoJSON only; KML has no thumbnail support, so the flag is ignored (with a warning) when combined with `--format kml`.

`scan` walks the tree in parallel and is typically the fastest option. `import` is useful when you already have a list of paths, or want to combine it with another tool; it does not walk the filesystem itself.

Run `img_coords` or `img_coords <COMMAND> --help` to get help.
