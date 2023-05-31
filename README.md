# ImageCoordinates
This executable can scan a directory tree on your disk, find all images with EXIF data containing coordinates, and collect them into a single file.
Output can be either [GeoJSON](https://geojson.org/) (default), or KML. JSON also contains the timestamp the image was taken, and the camera direction, if available in EXIF.
Use `--thumbnails` to add `base64`-encoded thumbnails to the `GeoJSON` output.

# Installation
```
cargo install img_coords
```
, _or_ download the binaries for your platform from the release

, _or_ checkout and build the repo manually

## Uninstall
```
cargo uninstall img_coords
```


# Example
Write a KML file to STDOUT:
```
img_coords scan --dir SOME_ROOT_DIRECTORY --format kml --thumbnails
```
Use a previously generated KML file and only scan/add files not in there:
```
img_coords scan --dir SOME_ROOT_DIRECTORY --format kml --update EXISTING.KML
```
Use `find` command (can be faster than the build-in `scan` command in some cases) to generate a file list:
```
find SOME_ROOT_DIRECTORY | img_coords import --format kml
```


Run `img_coords` or `img_coords scan` to get help.
